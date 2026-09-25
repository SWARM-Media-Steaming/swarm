//! Issue #442: "American Dad Show mouths are off bad" — audio/video sync
//! drifts over the course of an episode, reported on Season 1 Episode 1 but
//! "likely ... many more".
//!
//! Expected behavior, derived from the issue and playback/encoding domain
//! rules (not from the current implementation):
//!
//! 1. Old animated-TV DVD/broadcast rips (American Dad S1 is a 2005 Fox
//!    broadcast-era show) commonly carry a *variable* frame rate — telecine
//!    pulldown or re-timed sources where the container's nominal rate
//!    (`r_frame_rate`) and the stream's real average rate (`avg_frame_rate`)
//!    diverge. `swarm_media::probe::has_variable_frame_rate` must actually
//!    detect such a source as VFR, not merely fail to false-flag a
//!    well-behaved constant-rate source — a detector that never fires true
//!    is indistinguishable from no detector at all.
//! 2. The server's LAN "remux" fast path copies source video untouched
//!    (`-c:v copy`) while re-encoding audio fresh. For a VFR source this is
//!    exactly the mechanism the issue describes: irregular video frame
//!    timing survives the copy while the audio gets new constant-rate
//!    timestamps, and the two drift apart over the runtime of an episode.
//!    A LAN-eligible, otherwise remux-compatible VFR source must therefore
//!    be routed to the re-encode ladder instead of the remux path.
//! 3. That re-encode must actually normalize frame timing (`-fps_mode cfr`
//!    or equivalent) rather than merely picking a different code path that
//!    happens to sidestep the symptom without fixing it — a real ffmpeg
//!    process has to accept the command and flush a playable segment.
//! 4. This must not regress the common case: a constant-frame-rate source
//!    that is otherwise remux-eligible on LAN must still remux, not silently
//!    fall onto the more expensive re-encode ladder for every playback.
//! 5. Frame-rate detection reads real media with a real ffprobe subprocess.
//!    Any failure mode there (missing binary, no video stream, nonexistent
//!    file) must fail open (report "not VFR") rather than panic or hang —
//!    consistent with every other probe in this module — so a probe glitch
//!    degrades to the pre-#442 remux behavior instead of breaking playback
//!    outright.
//! 6. The user-visible invariant is lip-sync, not a playlist name: after
//!    the server prepares playback, audio and video must start together and
//!    must not accumulate a gap by the end of the stream. Animation lipsync
//!    breaks well before a tenth of a second of drift.
//! 7. American Dad S1 DVD rips are typically H.264 (or MPEG-2 historically)
//!    inside MKV with AC-3 5.1. Fire TV's baseline advertises `mp4`/`hls`
//!    only, so `prefer_direct` still falls through to HLS. 5.1 AC-3 is
//!    copied into fMP4 while video is re-encoded; that mixed path must
//!    still keep mouths on the dialogue.
//! 8. NTSC telecine mixes `24000/1001` and `30000/1001` (~20% rate
//!    divergence). A detector that only notices a synthetic 24+60 splice
//!    has not covered the reported title.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use swarm_core::capability::CapabilityProfile;
use swarm_core::peer::{AudioStreamInfo, MediaKind, PlaybackMode, PlaybackPreferences, VideoStreamInfo};
use swarm_media::store::EntryRecord;
use swarm_media::transcode::{TranscodeConfig, TranscodeManager, VideoEncoderMode};

static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

fn unique_dir(tag: &str) -> PathBuf {
    let n = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "swarm-adv-442-{tag}-{}-{n}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn ffmpeg_available() -> bool {
    std::process::Command::new("ffmpeg")
        .arg("-version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
        && std::process::Command::new("ffprobe")
            .arg("-version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
}

fn require_ffmpeg() {
    assert!(
        ffmpeg_available(),
        "ffmpeg and ffprobe are required for issue #442 adversarial playback tests"
    );
}

/// One constant-rate H.264+AAC segment. Used standalone as the CFR control
/// fixture, and as a building block concatenated with a different-rate
/// sibling to build the VFR fixture.
fn build_segment(dir: &Path, name: &str, rate: &str, tone_hz: u32, duration_secs: u32) -> PathBuf {
    build_segment_with_audio(dir, name, rate, tone_hz, duration_secs, "aac", 2, 96)
}

fn build_segment_with_audio(
    dir: &Path,
    name: &str,
    rate: &str,
    tone_hz: u32,
    duration_secs: u32,
    audio_codec: &str,
    channels: u32,
    audio_kbps: u32,
) -> PathBuf {
    let path = dir.join(name);
    let status = std::process::Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            &format!("testsrc2=size=320x240:rate={rate}:duration={duration_secs}"),
            "-f",
            "lavfi",
            "-i",
            &format!("sine=frequency={tone_hz}:duration={duration_secs}"),
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            audio_codec,
            "-ac",
            &channels.to_string(),
            "-b:a",
            &format!("{audio_kbps}k"),
            "-shortest",
            "-y",
        ])
        .arg(&path)
        .status()
        .unwrap();
    assert!(status.success(), "fixture encode of {name} failed");
    path
}

fn concat_copy(dir: &Path, segments: &[PathBuf], out_name: &str) -> PathBuf {
    let list = dir.join(format!("{out_name}.concat.txt"));
    let body = segments
        .iter()
        .map(|path| format!("file '{}'\n", path.display()))
        .collect::<String>();
    std::fs::write(&list, body).unwrap();
    let out = dir.join(out_name);
    let status = std::process::Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-f", "concat", "-safe", "0", "-i"])
        .arg(&list)
        .args(["-c", "copy", "-y"])
        .arg(&out)
        .status()
        .unwrap();
    assert!(status.success(), "concat of {out_name} failed");
    out
}

/// MP4 concat-copy of mixed-rate segments leaves video DTS longer than the
/// audio (the splice does not renormalize). Trim both streams to the
/// intended runtime so the *source* is in sync; the VFR divergence stays.
fn concat_copy_aligned(
    dir: &Path,
    segments: &[PathBuf],
    out_name: &str,
    duration_secs: u32,
) -> PathBuf {
    let raw = concat_copy(dir, segments, &format!("raw-{out_name}"));
    let out = dir.join(out_name);
    let status = std::process::Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-i"])
        .arg(&raw)
        .args(["-t", &duration_secs.to_string(), "-c", "copy", "-y"])
        .arg(&out)
        .status()
        .unwrap();
    assert!(status.success(), "align-trim of {out_name} failed");
    out
}

/// A real VFR container: two constant-rate segments (24fps, 60fps) spliced
/// with `-c copy`, which preserves each segment's original frame timing
/// instead of renormalizing it. This is the same shape of divergence a
/// telecined/re-timed broadcast rip produces — the container's declared
/// `r_frame_rate` reflects one timing regime while `avg_frame_rate` (total
/// frames over total duration) reflects the mixed reality.
fn build_vfr_fixture(dir: &Path) -> PathBuf {
    let seg_a = build_segment(dir, "seg_a.mp4", "24", 440, 1);
    let seg_b = build_segment(dir, "seg_b.mp4", "60", 220, 1);
    concat_copy_aligned(dir, &[seg_a, seg_b], "vfr.mp4", 2)
}

/// NTSC telecine-shaped hybrid: 23.976 fps film cadence spliced onto 29.97
/// fps video cadence. American Dad S1 DVD/broadcast rips are this mix
/// (credits, interstitials, and film-origin animation at different rates).
fn build_telecine_fixture(dir: &Path) -> PathBuf {
    let film = build_segment(dir, "film.mp4", "24000/1001", 440, 2);
    let video = build_segment(dir, "video.mp4", "30000/1001", 220, 2);
    concat_copy_aligned(dir, &[film, video], "telecine.mp4", 4)
}

/// Same telecine mix with DVD-style AC-3 5.1. Fire TV copies 5.1 AC-3 into
/// HLS, so video re-encode must not leave the copied audio behind.
fn build_telecine_ac3_51_fixture(dir: &Path) -> PathBuf {
    let film = build_segment_with_audio(dir, "film_ac3.mp4", "24000/1001", 440, 2, "ac3", 6, 192);
    let video = build_segment_with_audio(dir, "video_ac3.mp4", "30000/1001", 220, 2, "ac3", 6, 192);
    concat_copy_aligned(dir, &[film, video], "telecine_ac3.mp4", 4)
}

fn build_cfr_fixture(dir: &Path) -> PathBuf {
    build_segment(dir, "cfr.mp4", "24", 440, 2)
}

fn build_audio_only_fixture(dir: &Path) -> PathBuf {
    let path = dir.join("audio_only.m4a");
    let status = std::process::Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:duration=1",
            "-c:a",
            "aac",
            "-y",
        ])
        .arg(&path)
        .status()
        .unwrap();
    assert!(status.success(), "audio-only fixture encode failed");
    path
}

/// Independent parse of ffprobe's rational `r_frame_rate`/`avg_frame_rate`
/// output, kept separate from (and not calling into) the crate under test —
/// this exists purely to assert the fixtures actually have the timing
/// properties the tests below assume, the same way the issue #355 adversarial
/// suite asserts its NFC/NFD fixture strings are genuinely distinct before
/// relying on that distinction.
fn probe_video_rates(path: &Path) -> (f64, f64) {
    let output = std::process::Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=r_frame_rate,avg_frame_rate",
            "-of",
            "csv=p=0",
        ])
        .arg(path)
        .output()
        .unwrap();
    assert!(output.status.success(), "ffprobe failed on fixture {path:?}");
    let text = String::from_utf8_lossy(&output.stdout);
    let mut rates = text.trim().split([',', '\n']).map(|field| {
        let field = field.trim();
        let (num, den) = field.split_once('/').unwrap_or_else(|| {
            panic!("expected rational rate in {text:?}, got field {field:?}")
        });
        num.parse::<f64>().unwrap() / den.parse::<f64>().unwrap()
    });
    (rates.next().unwrap(), rates.next().unwrap())
}

fn probe_duration_secs(path: &Path) -> f64 {
    let output = std::process::Command::new("ffprobe")
        .args(["-v", "error", "-show_entries", "format=duration", "-of", "csv=p=0"])
        .arg(path)
        .output()
        .unwrap();
    assert!(output.status.success(), "ffprobe duration probe failed on {path:?}");
    String::from_utf8_lossy(&output.stdout).trim().parse().unwrap()
}

fn rate_divergence(nominal: f64, average: f64) -> f64 {
    (nominal - average).abs() / nominal.max(1.0)
}

fn entry_for(path: &Path, relative_path: &str) -> EntryRecord {
    entry_with_audio(path, relative_path, "aac", 1, 96_000)
}

fn entry_with_audio(
    path: &Path,
    relative_path: &str,
    audio_codec: &str,
    channels: u32,
    audio_bitrate: u64,
) -> EntryRecord {
    let duration_secs = probe_duration_secs(path);
    let size = std::fs::metadata(path).unwrap().len();
    EntryRecord {
        entry_key: format!("entry-{relative_path}"),
        relative_path: relative_path.into(),
        kind: MediaKind::Episode,
        title: "American Dad S1E1 (fixture)".into(),
        size,
        modified_time: 0,
        fingerprint: format!("fp-{relative_path}"),
        artist: None,
        album: None,
        track_number: None,
        show_title: Some("American Dad".into()),
        season: Some(1),
        episode: Some(1),
        year: Some(2005),
        duration_secs: Some(duration_secs),
        video: Some(VideoStreamInfo {
            codec: "h264".into(),
            width: 320,
            height: 240,
            bitrate: Some(400_000),
            ..Default::default()
        }),
        audio: Some(AudioStreamInfo {
            codec: audio_codec.into(),
            channels,
            bitrate: Some(audio_bitrate),
        }),
        scraped_title: None,
        episode_title: None,
        genres: vec![],
        artwork_version: 0,
        cast: vec![],
        overview: None,
        rating: None,
        community_rating: None,
        community_rating_votes: None,
        parent_entry_key: None,
        extra_type: None,
        extra_title: None,
        extra_relative_path: None,
        extra_category_path: None,
    }
}

/// `prefer_direct: false` pushes `plan()` past the direct-play branch so the
/// LAN remux-vs-reencode decision under test is actually reached, matching
/// the equivalent in-crate smoke test's `prefs.prefer_direct = false`.
fn lan_preferences() -> PlaybackPreferences {
    PlaybackPreferences {
        capabilities: CapabilityProfile::fire_tv_baseline(),
        start_position_secs: 0,
        prefer_direct: false,
        preview: false,
    }
}

fn transcode_manager(session_dir: PathBuf) -> std::sync::Arc<TranscodeManager> {
    TranscodeManager::new(TranscodeConfig {
        enabled: true,
        ffmpeg_path: "ffmpeg".into(),
        session_dir,
        max_upload_bps: 10_000_000,
        reserve_percent: 30,
        max_sessions: 2,
        idle_timeout: Duration::from_secs(300),
        segment_duration_secs: 4,
        video_encoder_mode: VideoEncoderMode::Software,
        ..Default::default()
    })
}

fn write_stub_ffmpeg_tree(dir: &Path, stdout: &str, exit_code: i32) -> PathBuf {
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let ffmpeg = bin.join("ffmpeg");
    let ffprobe = bin.join("ffprobe");
    let stdout_path = bin.join("ffprobe.stdout");
    std::fs::write(&stdout_path, stdout).unwrap();
    std::fs::write(&ffmpeg, "#!/bin/sh\nexit 127\n").unwrap();
    std::fs::write(
        &ffprobe,
        format!(
            "#!/bin/sh\ncat \"{}\"\nexit {}\n",
            stdout_path.display(),
            exit_code
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o755);
        std::fs::set_permissions(&ffmpeg, perms.clone()).unwrap();
        std::fs::set_permissions(&ffprobe, perms).unwrap();
    }
    ffmpeg
}

async fn detect_with_stub(stdout: &str, exit_code: i32) -> bool {
    let dir = unique_dir("stub");
    let ffmpeg = write_stub_ffmpeg_tree(&dir, stdout, exit_code);
    let media = dir.join("media.mp4");
    std::fs::write(&media, b"not a container").unwrap();
    let flagged = swarm_media::probe::has_variable_frame_rate(&ffmpeg, &media).await;
    let _ = std::fs::remove_dir_all(&dir);
    flagged
}

fn playlist_video_dir(master: &str) -> &'static str {
    if master.contains("v360p/index.m3u8") {
        "v360p"
    } else if master.contains("vsource/index.m3u8") {
        "vsource"
    } else {
        panic!("master playlist has neither v360p nor vsource: {master}");
    }
}

fn playlist_audio_dir(master: &str) -> String {
    master
        .lines()
        .find(|line| line.contains("TYPE=AUDIO") && line.contains("URI=\""))
        .and_then(|line| {
            line.split("URI=\"")
                .nth(1)
                .and_then(|rest| rest.split('"').next())
        })
        .and_then(|uri| uri.split('/').next())
        .map(str::to_string)
        .unwrap_or_else(|| panic!("master playlist has no audio URI: {master}"))
}

async fn wait_for_endlist(playlist: &Path) {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        if let Ok(text) = std::fs::read_to_string(playlist) {
            if text.contains("#EXT-X-ENDLIST") {
                return;
            }
        }
        if Instant::now() >= deadline {
            let body = std::fs::read_to_string(playlist).unwrap_or_default();
            panic!("timed out waiting for #EXT-X-ENDLIST in {playlist:?}: {body}");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn stitch_fmp4_playlist(playlist: &Path) -> PathBuf {
    let dir = playlist.parent().expect("playlist has parent");
    let text = std::fs::read_to_string(playlist).unwrap_or_else(|_| {
        panic!("missing playlist {playlist:?}")
    });
    let init = text
        .lines()
        .find_map(|line| line.strip_prefix("#EXT-X-MAP:URI=\""))
        .and_then(|rest| rest.split('"').next())
        .unwrap_or_else(|| panic!("playlist has no #EXT-X-MAP: {text}"));
    let mut bytes = std::fs::read(dir.join(init)).unwrap();
    let mut segments = 0usize;
    for line in text.lines() {
        if line.ends_with(".m4s") {
            bytes.extend(std::fs::read(dir.join(line)).unwrap());
            segments += 1;
        }
    }
    assert!(segments > 0, "playlist has no media segments: {text}");
    let out = dir.join(format!(
        "{}-stitched.mp4",
        dir.file_name().unwrap().to_string_lossy()
    ));
    std::fs::write(&out, bytes).unwrap();
    out
}

fn packet_span(path: &Path, stream: &str) -> (f64, f64) {
    let output = std::process::Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            stream,
            "-show_entries",
            "packet=pts_time,duration_time",
            "-of",
            "csv=p=0",
        ])
        .arg(path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "ffprobe packet dump failed on {path:?} {stream}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let mut first = None;
    let mut last_end = None;
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let mut fields = line.split([',', '\n']).filter_map(|field| {
            let field = field.trim();
            if field.is_empty() {
                None
            } else {
                field.parse::<f64>().ok()
            }
        });
        let Some(pts) = fields.next() else {
            continue;
        };
        if !pts.is_finite() {
            continue;
        }
        let duration = fields.next().filter(|value| value.is_finite()).unwrap_or(0.0);
        if first.is_none() {
            first = Some(pts);
        }
        last_end = Some(pts + duration);
    }
    (
        first.unwrap_or_else(|| panic!("no packets for {stream} in {path:?}")),
        last_end.unwrap_or_else(|| panic!("no packet span for {stream} in {path:?}")),
    )
}

/// Perceptible animation lipsync fails around 80–100 ms. AAC priming is
/// ~20–40 ms, so a tenth of a second at the start still catches "mouths
/// off bad" without flaking on encoder delay. End-of-stream gap is the
/// drift the issue reports over an episode; on a few-second fixture any
/// growing gap is already a failure.
const START_SYNC_SECS: f64 = 0.10;
const END_DRIFT_SECS: f64 = 0.15;
const CFR_DIVERGENCE: f64 = 0.02;

fn assert_output_is_cfr_and_in_sync(session_dir: &Path, master: &str, label: &str) {
    let video_dir = playlist_video_dir(master);
    let audio_dir = playlist_audio_dir(master);
    let video = stitch_fmp4_playlist(&session_dir.join(video_dir).join("index.m3u8"));
    let audio = stitch_fmp4_playlist(&session_dir.join(&audio_dir).join("index.m3u8"));

    let (nominal, average) = probe_video_rates(&video);
    let divergence = rate_divergence(nominal, average);
    assert!(
        divergence < CFR_DIVERGENCE,
        "{label}: playback video must be constant-rate so a client that keys off \
         r_frame_rate does not run mouths at the wrong speed; r={nominal} avg={average} \
         divergence={divergence}"
    );

    let (v_start, v_end) = packet_span(&video, "v:0");
    let (a_start, a_end) = packet_span(&audio, "a:0");
    let start_gap = (v_start - a_start).abs();
    let end_gap = (v_end - a_end).abs();
    assert!(
        start_gap < START_SYNC_SECS,
        "{label}: audio and video must start together (mouths on the first line); \
         video_start={v_start} audio_start={a_start} gap={start_gap}"
    );
    assert!(
        end_gap < END_DRIFT_SECS,
        "{label}: audio and video must not drift apart by the end of the stream \
         (the American Dad S1 mouths-off failure mode); video_end={v_end} \
         audio_end={a_end} gap={end_gap}"
    );
}

#[test]
fn fixture_invariant_vfr_source_diverges_far_past_the_two_percent_threshold() {
    require_ffmpeg();
    let dir = unique_dir("fixture-invariant");
    let vfr = build_vfr_fixture(&dir);
    let cfr = build_cfr_fixture(&dir);

    let (vfr_nominal, vfr_avg) = probe_video_rates(&vfr);
    let vfr_divergence = (vfr_nominal - vfr_avg).abs() / vfr_nominal;
    assert!(
        vfr_divergence > 0.02,
        "fixture invariant: spliced 24fps+60fps source must diverge past the 2% VFR \
         threshold to be a meaningful test of detection, got r={vfr_nominal} avg={vfr_avg} \
         (divergence {vfr_divergence})"
    );

    let (cfr_nominal, cfr_avg) = probe_video_rates(&cfr);
    let cfr_divergence = (cfr_nominal - cfr_avg).abs() / cfr_nominal.max(1.0);
    assert!(
        cfr_divergence < 0.02,
        "fixture invariant: a plain constant-rate encode must not itself diverge, \
         got r={cfr_nominal} avg={cfr_avg}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn has_variable_frame_rate_flags_real_vfr_source_and_clears_real_cfr_source() {
    require_ffmpeg();
    let dir = unique_dir("detect");
    let vfr = build_vfr_fixture(&dir);
    let cfr = build_cfr_fixture(&dir);
    let ffmpeg_path = PathBuf::from("ffmpeg");

    assert!(
        swarm_media::probe::has_variable_frame_rate(&ffmpeg_path, &vfr).await,
        "a real spliced-rate source (the American Dad S1 shape of file, #442) must be \
         detected as VFR — a detector that only ever proves the negative case is not \
         actually protecting anything"
    );
    assert!(
        !swarm_media::probe::has_variable_frame_rate(&ffmpeg_path, &cfr).await,
        "a genuinely constant-rate source must not be flagged as VFR"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn has_variable_frame_rate_fails_open_when_ffprobe_binary_is_missing() {
    let dir = unique_dir("missing-binary");
    // Doesn't need to exist as media — the probe must fail before ever
    // reading its contents, because the ffprobe binary itself can't run.
    let bogus_media = dir.join("whatever.mp4");
    std::fs::write(&bogus_media, b"not a real media file").unwrap();
    let bogus_ffmpeg = dir.join("no-such-ffmpeg-binary-here");

    assert!(
        !swarm_media::probe::has_variable_frame_rate(&bogus_ffmpeg, &bogus_media).await,
        "a missing ffprobe binary must fail open (assume CFR), not panic"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn has_variable_frame_rate_fails_open_for_audio_only_source_without_video_stream() {
    require_ffmpeg();
    let dir = unique_dir("audio-only");
    let audio_only = build_audio_only_fixture(&dir);
    let ffmpeg_path = PathBuf::from("ffmpeg");

    assert!(
        !swarm_media::probe::has_variable_frame_rate(&ffmpeg_path, &audio_only).await,
        "a source with no v:0 stream (music, or a stripped/corrupt video track) must \
         fail open rather than panic on the empty ffprobe csv output"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn has_variable_frame_rate_fails_open_for_nonexistent_media_file() {
    let dir = unique_dir("nonexistent");
    let missing = dir.join("does-not-exist.mp4");
    let ffmpeg_path = PathBuf::from("ffmpeg");

    assert!(
        !swarm_media::probe::has_variable_frame_rate(&ffmpeg_path, &missing).await,
        "ffprobe failing on a nonexistent path must fail open, not panic"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// Distinct from the synthetic-CSV `detector_fails_open_on_malformed_ffprobe_csv`
/// stub cases: this exercises the real, non-stubbed ffprobe binary against a
/// genuinely truncated MP4 (moov atom missing — the shape of a partially
/// downloaded or SMB-interrupted rip, per this repo's history of flaky-share
/// media). Verified independently that real ffprobe reports a fast non-zero
/// exit here (`Invalid data found when processing input`), well under a
/// second — so the assertion below is about correctness of the fail-open
/// path, not a timing/hang risk.
#[tokio::test]
async fn has_variable_frame_rate_fails_open_for_truncated_real_media_file() {
    require_ffmpeg();
    let dir = unique_dir("truncated");
    let source = build_cfr_fixture(&dir);
    let truncated = dir.join("truncated.mp4");
    let full = std::fs::read(&source).unwrap();
    // Cut off well before the moov atom that trails a freshly muxed mp4, so
    // ffprobe can't find stream metadata at all.
    let cut = full.len().min(3_000);
    std::fs::write(&truncated, &full[..cut]).unwrap();
    let ffmpeg_path = PathBuf::from("ffmpeg");

    assert!(
        !swarm_media::probe::has_variable_frame_rate(&ffmpeg_path, &truncated).await,
        "a truncated/corrupt real media file (partial download or interrupted SMB copy) \
         must fail open, not panic or misreport VFR"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn lan_remux_eligible_vfr_source_routes_to_reencode_ladder_with_normalized_timing() {
    require_ffmpeg();
    let dir = unique_dir("plan-vfr");
    let source = build_vfr_fixture(&dir);
    let entry = entry_for(&source, "shows/American Dad/S01E01.mp4");
    let manager = transcode_manager(dir.join("sessions"));

    let plan = manager
        .plan(&entry, &source, &lan_preferences(), true, None)
        .await
        .unwrap();
    assert_eq!(plan.mode, PlaybackMode::Hls);

    let relative = plan.path.splitn(4, '/').nth(3).unwrap();
    let session = plan.path.split('/').nth(2).unwrap();
    let file = manager.open_hls(session, relative).unwrap();
    let master = std::fs::read_to_string(&file.path).unwrap();

    assert!(
        !master.contains("vsource/index.m3u8"),
        "a VFR source must not take the remux (`-c:v copy`) fast path — that's the exact \
         mechanism #442 reports (copied irregular video timing vs. freshly-timed re-encoded \
         audio drifting apart over an episode); master playlist was: {master}"
    );
    assert!(
        master.contains("v360p/index.m3u8"),
        "a VFR source falling through the remux check must land on the re-encode ladder \
         (only the 360p rung is eligible for this tiny fixture); master playlist was: {master}"
    );

    let rung_playlist =
        std::fs::read_to_string(file.path.parent().unwrap().join("v360p/index.m3u8")).unwrap();
    assert!(
        rung_playlist.contains("#EXTINF"),
        "real ffmpeg must accept the re-encode command (including `-fps_mode cfr`) and \
         flush a playable segment, not just be routed here in theory: {rung_playlist}"
    );

    manager.finish_use(session);
    manager.release(session);
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn lan_remux_eligible_cfr_source_still_remuxes_without_reencode() {
    require_ffmpeg();
    let dir = unique_dir("plan-cfr");
    let source = build_cfr_fixture(&dir);
    let entry = entry_for(&source, "shows/American Dad/S01E02.mp4");
    let manager = transcode_manager(dir.join("sessions"));

    let plan = manager
        .plan(&entry, &source, &lan_preferences(), true, None)
        .await
        .unwrap();
    assert_eq!(plan.mode, PlaybackMode::Hls);

    let relative = plan.path.splitn(4, '/').nth(3).unwrap();
    let session = plan.path.split('/').nth(2).unwrap();
    let file = manager.open_hls(session, relative).unwrap();
    let master = std::fs::read_to_string(&file.path).unwrap();

    assert!(
        master.contains("vsource/index.m3u8"),
        "a constant-frame-rate, otherwise remux-eligible source on LAN must still take the \
         cheap `-c:v copy` remux path — the #442 fix must not regress this into always \
         re-encoding; master playlist was: {master}"
    );
    assert!(
        !master.contains("v360p/index.m3u8"),
        "a remuxed source must not also carry a re-encoded ladder rung: {master}"
    );

    let source_playlist =
        std::fs::read_to_string(file.path.parent().unwrap().join("vsource/index.m3u8")).unwrap();
    assert!(
        source_playlist.contains("#EXTINF"),
        "real ffmpeg must accept `-c:v copy` and flush a playable segment: {source_playlist}"
    );

    manager.finish_use(session);
    manager.release(session);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn fixture_invariant_ntsc_telecine_mix_matches_american_dad_s1_shape() {
    require_ffmpeg();
    let dir = unique_dir("telecine-invariant");
    let telecine = build_telecine_fixture(&dir);
    let (nominal, average) = probe_video_rates(&telecine);
    let divergence = rate_divergence(nominal, average);
    assert!(
        divergence > 0.02,
        "fixture invariant: 24000/1001 spliced onto 30000/1001 must look like a telecined \
         American Dad S1 rip to the detector (r vs avg > 2%), got r={nominal} avg={average} \
         divergence={divergence}"
    );
    let (v_start, v_end) = packet_span(&telecine, "v:0");
    let (a_start, a_end) = packet_span(&telecine, "a:0");
    assert!(
        (v_start - a_start).abs() < START_SYNC_SECS
            && (v_end - a_end).abs() < END_DRIFT_SECS,
        "fixture invariant: the telecine source itself must be in sync before we blame \
         the server (else the test is measuring concat-copy DTS gaps); \
         video={v_start}..{v_end} audio={a_start}..{a_end}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn has_variable_frame_rate_flags_ntsc_telecine_mix() {
    require_ffmpeg();
    let dir = unique_dir("telecine-detect");
    let telecine = build_telecine_fixture(&dir);
    assert!(
        swarm_media::probe::has_variable_frame_rate(&PathBuf::from("ffmpeg"), &telecine).await,
        "a 23.976/29.97 hybrid (American Dad S1 telecine shape) must be detected as VFR"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn detector_treats_exact_two_percent_slack_as_constant_rate() {
    // (50 - 49) / 50 == 0.02, and the product uses a strict `>` so rounding
    // of well-behaved CFR rationals must not trip the expensive ladder.
    assert!(
        !detect_with_stub("50/1,49/1\n", 0).await,
        "exactly 2% divergence is the documented slack for ffprobe rounding, not VFR"
    );
}

#[tokio::test]
async fn detector_flags_just_over_two_percent_as_variable() {
    // (50 - 48) / 50 == 0.04.
    assert!(
        detect_with_stub("50/1,48/1\n", 0).await,
        "a source whose average rate diverges just past the 2% slack must be VFR"
    );
}

#[tokio::test]
async fn detector_flags_canonical_ntsc_telecine_rationals() {
    assert!(
        detect_with_stub("30000/1001,24000/1001\n", 0).await,
        "r=29.97 avg=23.976 is the textbook telecine mix and must be VFR"
    );
}

#[tokio::test]
async fn detector_parses_whitespace_and_crlf_wrapped_csv() {
    assert!(
        detect_with_stub("  30000/1001 , 24000/1001 \r\n", 0).await,
        "ffprobe csv with spaces and CRLF must still detect telecine VFR"
    );
}

#[tokio::test]
async fn detector_fails_open_on_malformed_ffprobe_csv() {
    for (label, stdout, exit) in [
        ("empty", "", 0),
        ("n/a", "N/A,N/A\n", 0),
        ("integer-without-slash", "30\n", 0),
        ("unknown-rational", "0/0,24/1\n", 0),
        ("zero-average", "24/1,0/0\n", 0),
        ("single-field", "24/1\n", 0),
        ("negative", "-24/1,24/1\n", 0),
        ("nonzero-exit", "30000/1001,24000/1001\n", 1),
    ] {
        assert!(
            !detect_with_stub(stdout, exit).await,
            "malformed ffprobe output ({label}) must fail open rather than panic or false-positive"
        );
    }
}

#[tokio::test]
async fn fire_tv_mkv_ac3_51_telecine_episode_is_not_remuxed_even_when_client_prefers_direct() {
    require_ffmpeg();
    let dir = unique_dir("mkv-ac3");
    let source = build_telecine_ac3_51_fixture(&dir);
    // Library path is `.mkv`: Fire TV baseline has no mkv container, so
    // prefer_direct still falls through to the LAN remux decision — the
    // actual American Dad S1 DVD-rip path.
    let entry = entry_with_audio(
        &source,
        "TV Shows/American Dad/Season 01/American Dad - S01E01.mkv",
        "ac3",
        6,
        192_000,
    );
    let manager = transcode_manager(dir.join("sessions"));
    let prefs = PlaybackPreferences {
        capabilities: CapabilityProfile::fire_tv_baseline(),
        start_position_secs: 0,
        prefer_direct: true,
        preview: false,
    };

    let plan = manager
        .plan(&entry, &source, &prefs, true, None)
        .await
        .unwrap();
    assert_eq!(plan.mode, PlaybackMode::Hls);
    let relative = plan.path.splitn(4, '/').nth(3).unwrap();
    let session = plan.path.split('/').nth(2).unwrap().to_string();
    let file = manager.open_hls(&session, relative).unwrap();
    let master = std::fs::read_to_string(&file.path).unwrap();
    assert!(
        !master.contains("vsource/index.m3u8"),
        "Fire TV + MKV + AC-3 5.1 telecine (American Dad S1) must not copy VFR video \
         into HLS; master={master}"
    );
    assert!(
        master.contains("v360p/index.m3u8"),
        "that title must land on the re-encode ladder; master={master}"
    );

    let session_dir = file.path.parent().unwrap().to_path_buf();
    wait_for_endlist(&session_dir.join("v360p/index.m3u8")).await;
    let audio_dir = playlist_audio_dir(&master);
    wait_for_endlist(&session_dir.join(&audio_dir).join("index.m3u8")).await;
    assert_output_is_cfr_and_in_sync(&session_dir, &master, "mkv-ac3-51-telecine");

    manager.finish_use(&session);
    manager.release(&session);
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn lan_and_wan_telecine_reencode_keeps_mouths_on_the_dialogue() {
    require_ffmpeg();
    let dir = unique_dir("telecine-sync");
    let source = build_telecine_fixture(&dir);
    let entry = entry_for(&source, "shows/American Dad/S01E01.mp4");
    let manager = transcode_manager(dir.join("sessions"));
    let prefs = lan_preferences();

    for is_lan in [true, false] {
        let label = if is_lan { "lan-telecine" } else { "wan-telecine" };
        let plan = manager
            .plan(&entry, &source, &prefs, is_lan, None)
            .await
            .unwrap();
        assert_eq!(plan.mode, PlaybackMode::Hls, "{label}");
        let relative = plan.path.splitn(4, '/').nth(3).unwrap();
        let session = plan.path.split('/').nth(2).unwrap().to_string();
        let file = manager.open_hls(&session, relative).unwrap();
        let master = std::fs::read_to_string(&file.path).unwrap();
        assert!(
            !master.contains("vsource/index.m3u8"),
            "{label} must not remux VFR video; master={master}"
        );
        assert!(
            master.contains("v360p/index.m3u8"),
            "{label} must re-encode; master={master}"
        );
        let session_dir = file.path.parent().unwrap().to_path_buf();
        wait_for_endlist(&session_dir.join("v360p/index.m3u8")).await;
        let audio_dir = playlist_audio_dir(&master);
        wait_for_endlist(&session_dir.join(&audio_dir).join("index.m3u8")).await;
        assert_output_is_cfr_and_in_sync(&session_dir, &master, label);
        manager.finish_use(&session);
        manager.release(&session);
    }

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn mid_episode_start_on_telecine_keeps_audio_and_video_together() {
    require_ffmpeg();
    let dir = unique_dir("telecine-seek");
    // Longer hybrid so a start_position of 2s lands inside the second rate.
    let film = build_segment(&dir, "film.mp4", "24000/1001", 440, 3);
    let video = build_segment(&dir, "video.mp4", "30000/1001", 220, 3);
    let source = concat_copy_aligned(&dir, &[film, video], "telecine_long.mp4", 6);
    let entry = entry_for(&source, "shows/American Dad/S01E01.mp4");
    let manager = transcode_manager(dir.join("sessions"));
    let mut prefs = lan_preferences();
    prefs.start_position_secs = 2;

    let plan = manager
        .plan(&entry, &source, &prefs, true, None)
        .await
        .unwrap();
    let relative = plan.path.splitn(4, '/').nth(3).unwrap();
    let session = plan.path.split('/').nth(2).unwrap().to_string();
    let file = manager.open_hls(&session, relative).unwrap();
    let master = std::fs::read_to_string(&file.path).unwrap();
    assert!(
        !master.contains("vsource/index.m3u8"),
        "seeking into a telecine episode must still refuse VFR remux; master={master}"
    );
    let session_dir = file.path.parent().unwrap().to_path_buf();
    wait_for_endlist(&session_dir.join("v360p/index.m3u8")).await;
    let audio_dir = playlist_audio_dir(&master);
    wait_for_endlist(&session_dir.join(&audio_dir).join("index.m3u8")).await;
    assert_output_is_cfr_and_in_sync(&session_dir, &master, "telecine-mid-episode");

    manager.finish_use(&session);
    manager.release(&session);
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn cfr_lan_remux_output_stays_in_sync() {
    require_ffmpeg();
    let dir = unique_dir("cfr-sync");
    let source = build_cfr_fixture(&dir);
    let entry = entry_for(&source, "shows/American Dad/S01E02.mp4");
    let manager = transcode_manager(dir.join("sessions"));
    let plan = manager
        .plan(&entry, &source, &lan_preferences(), true, None)
        .await
        .unwrap();
    let relative = plan.path.splitn(4, '/').nth(3).unwrap();
    let session = plan.path.split('/').nth(2).unwrap().to_string();
    let file = manager.open_hls(&session, relative).unwrap();
    let master = std::fs::read_to_string(&file.path).unwrap();
    assert!(
        master.contains("vsource/index.m3u8"),
        "CFR control must still remux; master={master}"
    );
    let session_dir = file.path.parent().unwrap().to_path_buf();
    wait_for_endlist(&session_dir.join("vsource/index.m3u8")).await;
    let audio_dir = playlist_audio_dir(&master);
    wait_for_endlist(&session_dir.join(&audio_dir).join("index.m3u8")).await;
    assert_output_is_cfr_and_in_sync(&session_dir, &master, "cfr-remux");

    manager.finish_use(&session);
    manager.release(&session);
    let _ = std::fs::remove_dir_all(&dir);
}
