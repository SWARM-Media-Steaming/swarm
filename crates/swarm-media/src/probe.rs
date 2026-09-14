//! ffprobe wrapper — captures codec/container facts at scan time so
//! direct-play decisions never have to touch the file again. ffprobe is
//! optional: absence (or any failure) degrades to "no stream info", which
//! Phase 5 treats as "must transcode to be safe" for video.

use serde::Deserialize;
use std::path::{Path, PathBuf};
use swarm_core::peer::{AudioStreamInfo, VideoStreamInfo};

#[derive(Debug, Clone, Default, PartialEq)]
pub struct MediaInfo {
    pub duration_secs: Option<f64>,
    pub video: Option<VideoStreamInfo>,
    pub audio: Option<AudioStreamInfo>,
}

#[derive(Deserialize)]
struct FfprobeOutput {
    #[serde(default)]
    streams: Vec<FfprobeStream>,
    format: Option<FfprobeFormat>,
}

#[derive(Deserialize)]
struct FfprobeStream {
    index: Option<usize>,
    codec_type: Option<String>,
    codec_name: Option<String>,
    /// ffprobe usually reports this as a string ("High", "Main 10") but a few
    /// codecs/versions emit a bare number — accept either so one odd stream
    /// never sinks the whole probe.
    #[serde(default, deserialize_with = "deserialize_stringy_option")]
    profile: Option<String>,
    width: Option<u32>,
    height: Option<u32>,
    level: Option<i64>,
    #[serde(default)]
    pix_fmt: Option<String>,
    #[serde(default)]
    color_transfer: Option<String>,
    channels: Option<u32>,
    bit_rate: Option<String>,
    #[serde(default)]
    tags: FfprobeTags,
    #[serde(default)]
    disposition: FfprobeDisposition,
}

fn deserialize_stringy_option<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(match Option::<serde_json::Value>::deserialize(deserializer)? {
        Some(serde_json::Value::String(value)) => Some(value),
        Some(serde_json::Value::Number(value)) => Some(value.to_string()),
        _ => None,
    })
}

/// 10/12-bit pixel formats (`yuv420p10le`, `p010le`, `yuv444p12le`, …) all
/// carry the bit count as digits right after the plane layout.
fn bit_depth_from_pix_fmt(pix_fmt: &str) -> Option<u8> {
    if pix_fmt.contains("12") {
        Some(12)
    } else if pix_fmt.contains("10") {
        Some(10)
    } else if pix_fmt.is_empty() {
        None
    } else {
        Some(8)
    }
}

/// PQ (`smpte2084`) and HLG (`arib-std-b67`) are the two HDR transfer
/// characteristics ffprobe reports; everything else is SDR.
fn is_hdr_transfer(color_transfer: &str) -> bool {
    matches!(
        color_transfer.trim().to_ascii_lowercase().as_str(),
        "smpte2084" | "arib-std-b67"
    )
}

#[derive(Default, Deserialize)]
struct FfprobeTags {
    language: Option<String>,
    title: Option<String>,
    name: Option<String>,
    handler_name: Option<String>,
}

#[derive(Default, Deserialize)]
struct FfprobeDisposition {
    default: Option<i32>,
}

#[derive(Deserialize)]
struct FfprobeFormat {
    duration: Option<String>,
    bit_rate: Option<String>,
}

pub async fn probe(path: &Path) -> Option<MediaInfo> {
    let output = tokio::process::Command::new(resolve_ffprobe_path())
        .args([
            "-v",
            "quiet",
            "-print_format",
            "json",
            "-show_streams",
            "-show_format",
        ])
        .arg(path)
        .output()
        .await
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let parsed: FfprobeOutput = serde_json::from_slice(&output.stdout).ok()?;
    let mut info = MediaInfo {
        duration_secs: parsed
            .format
            .as_ref()
            .and_then(|f| f.duration.as_deref()?.parse().ok()),
        ..Default::default()
    };
    let container_bitrate: Option<u64> = parsed
        .format
        .as_ref()
        .and_then(|f| f.bit_rate.as_deref()?.parse().ok());
    for stream in parsed.streams {
        match stream.codec_type.as_deref() {
            Some("video") if info.video.is_none() => {
                info.video = Some(VideoStreamInfo {
                    codec: stream.codec_name.unwrap_or_default(),
                    width: stream.width.unwrap_or(0),
                    height: stream.height.unwrap_or(0),
                    // ffprobe reports H.264 level 4.1 as 41.
                    level: stream.level.map(|l| format!("{}.{}", l / 10, l % 10)),
                    bitrate: stream
                        .bit_rate
                        .as_deref()
                        .and_then(|b| b.parse().ok())
                        .or(container_bitrate),
                    profile: stream
                        .profile
                        .as_deref()
                        .map(str::trim)
                        .filter(|value| !value.is_empty() && !value.eq_ignore_ascii_case("unknown"))
                        .map(str::to_string),
                    bit_depth: stream
                        .pix_fmt
                        .as_deref()
                        .and_then(bit_depth_from_pix_fmt),
                    hdr: stream
                        .color_transfer
                        .as_deref()
                        .map(is_hdr_transfer),
                });
            }
            Some("audio") if info.audio.is_none() => {
                info.audio = Some(AudioStreamInfo {
                    codec: stream.codec_name.unwrap_or_default(),
                    channels: stream.channels.unwrap_or(0),
                    bitrate: stream.bit_rate.as_deref().and_then(|b| b.parse().ok()),
                });
            }
            _ => {}
        }
    }
    Some(info)
}

/// One embedded audio stream, as ffmpeg can map it (`0:{index}`) into an HLS
/// output. `is_preferred` marks the single track [select_preferred_audio_stream]
/// would pick, so the caller can flag it `default:yes` in the HLS audio group
/// without re-deriving the same English/container-default/first-track rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioStreamOption {
    pub index: usize,
    pub language: Option<String>,
    pub is_preferred: bool,
    /// ffprobe `codec_name` (`aac`, `ac3`, `eac3`, `dts`, …). Empty when the
    /// probe could not name it — the caller then transcodes rather than risk
    /// an unsupported passthrough.
    pub codec: String,
    /// Channel count; `0` when unknown. `> 2` is what makes a track worth
    /// preserving instead of downmixing to stereo.
    pub channels: u32,
}

/// Every embedded audio stream ffmpeg can map, in container order. Empty on
/// any probe failure so playback can fall back to ffmpeg's own `0:a:0`
/// selection rather than mapping nothing.
pub async fn list_audio_streams(ffmpeg_path: &Path, media_path: &Path) -> Vec<AudioStreamOption> {
    let Ok(output) = tokio::process::Command::new(ffprobe_path_for(ffmpeg_path))
        .args([
            "-v",
            "quiet",
            "-print_format",
            "json",
            "-select_streams",
            "a",
            "-show_entries",
            "stream=index,codec_type,codec_name,channels:stream_tags=language,title,name,handler_name:stream_disposition=default",
        ])
        .arg(media_path)
        .output()
        .await
    else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    let Ok(parsed) = serde_json::from_slice::<FfprobeOutput>(&output.stdout) else {
        return Vec::new();
    };
    let preferred = select_preferred_audio_stream(&parsed.streams);
    parsed
        .streams
        .iter()
        .filter(|stream| stream.codec_type.as_deref() == Some("audio"))
        .filter_map(|stream| {
            let index = stream.index?;
            Some(AudioStreamOption {
                index,
                language: audio_stream_language(&stream.tags),
                is_preferred: Some(index) == preferred,
                codec: stream.codec_name.clone().unwrap_or_default(),
                channels: stream.channels.unwrap_or(0),
            })
        })
        .collect()
}

/// Largest video keyframe timestamp at or before `target_secs`.
///
/// A mid-file transcode seek is normally passed to FFmpeg as a single input
/// `-ss`. When the video is re-encoded but an audio track is stream-copied
/// (AC-3/E-AC-3/AAC surround the client can take untouched), FFmpeg rebases the
/// re-encoded video to the keyframe it decoded from while rebasing the copied
/// audio to the exact seek point — so a hover preview plays several seconds of
/// video before its sound catches up (#226). Knowing the real keyframe lets the
/// caller split the seek into a fast input `-ss` to this keyframe plus a short
/// output `-ss` for the remainder, which trims both streams to the same point.
///
/// `None` on any probe failure (ffprobe missing, slow share, no keyframe found):
/// the caller then falls back to the plain single input seek.
pub async fn keyframe_at_or_before(
    ffmpeg_path: &Path,
    media_path: &Path,
    target_secs: u64,
) -> Option<f64> {
    if target_secs == 0 {
        return None;
    }
    // Movie GOPs are almost always a few seconds; 15 covers the outliers while
    // bounding how much video FFmpeg has to decode before the first segment.
    let window_start = target_secs.saturating_sub(15);
    let output = tokio::process::Command::new(ffprobe_path_for(ffmpeg_path))
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-skip_frame",
            "nokey",
            "-show_entries",
            "frame=pts_time,best_effort_timestamp_time",
            "-of",
            "csv=p=0",
            "-read_intervals",
        ])
        .arg(format!("{window_start}%{target_secs}"))
        .arg(media_path)
        .output()
        .await
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let ceiling = target_secs as f64 + 0.001;
    String::from_utf8_lossy(&output.stdout)
        .split([',', '\n'])
        .filter_map(|field| field.trim().parse::<f64>().ok())
        .filter(|value| value.is_finite() && *value <= ceiling)
        .fold(None, |best: Option<f64>, value| {
            Some(best.map_or(value, |current| current.max(value)))
        })
}

fn ffprobe_path_for(ffmpeg_path: &Path) -> PathBuf {
    if let Some(configured) = std::env::var_os("SWARM_FFPROBE_PATH") {
        return PathBuf::from(configured);
    }
    let has_explicit_parent = ffmpeg_path
        .parent()
        .is_some_and(|parent| !parent.as_os_str().is_empty());
    if !has_explicit_parent {
        return resolve_ffprobe_path();
    }
    let extension = ffmpeg_path.extension();
    let name = if extension.is_some_and(|value| value.eq_ignore_ascii_case("exe")) {
        "ffprobe.exe"
    } else {
        "ffprobe"
    };
    ffmpeg_path.with_file_name(name)
}

/// Locate `ffprobe`, tolerating the reduced `PATH` a macOS GUI app inherits.
///
/// A media server opened from Finder or launched at login gets a much smaller
/// `PATH` than an interactive shell, so a Homebrew or MacPorts `ffprobe` can be
/// installed and still be invisible — the same failure the server resolves for
/// `ffmpeg` (#203), which left every scanned entry with no codec/duration facts
/// and, downstream, no audio in HLS playback. Honour `SWARM_FFPROBE_PATH`
/// first, then an `ffprobe` sitting next to an explicit `SWARM_FFMPEG_PATH`,
/// then the inherited `PATH`, then the common package-manager locations used by
/// both Apple Silicon and Intel Macs.
fn resolve_ffprobe_path() -> PathBuf {
    #[cfg(target_os = "macos")]
    let platform_candidates = [
        PathBuf::from("/opt/homebrew/bin/ffprobe"),
        PathBuf::from("/usr/local/bin/ffprobe"),
        PathBuf::from("/opt/local/bin/ffprobe"),
    ];
    #[cfg(not(target_os = "macos"))]
    let platform_candidates: [PathBuf; 0] = [];

    let configured = std::env::var_os("SWARM_FFPROBE_PATH").or_else(|| {
        let ffmpeg = PathBuf::from(std::env::var_os("SWARM_FFMPEG_PATH")?);
        ffmpeg
            .parent()
            .is_some_and(|parent| !parent.as_os_str().is_empty())
            .then(|| ffprobe_path_for(&ffmpeg).into_os_string())
    });

    resolve_ffprobe_path_from(configured, std::env::var_os("PATH"), &platform_candidates)
}

fn resolve_ffprobe_path_from(
    configured: Option<std::ffi::OsString>,
    search_path: Option<std::ffi::OsString>,
    platform_candidates: &[PathBuf],
) -> PathBuf {
    if let Some(configured) = configured {
        return PathBuf::from(configured);
    }

    let executable_name = if cfg!(windows) {
        "ffprobe.exe"
    } else {
        "ffprobe"
    };

    search_path
        .as_deref()
        .into_iter()
        .flat_map(std::env::split_paths)
        .map(|directory| directory.join(executable_name))
        .chain(platform_candidates.iter().cloned())
        .find(|candidate| is_executable_file(candidate))
        .unwrap_or_else(|| PathBuf::from(executable_name))
}

fn is_executable_file(path: &Path) -> bool {
    let Ok(metadata) = path.metadata() else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

fn select_preferred_audio_stream(streams: &[FfprobeStream]) -> Option<usize> {
    let audio = streams
        .iter()
        .filter(|stream| stream.codec_type.as_deref() == Some("audio"));
    audio
        .clone()
        .filter(|stream| {
            audio_stream_language(&stream.tags)
                .as_deref()
                .is_some_and(is_english)
        })
        .max_by_key(|stream| stream.disposition.default.unwrap_or(0))
        .and_then(|stream| stream.index)
        .or_else(|| {
            audio
                .clone()
                .find(|stream| stream.disposition.default.unwrap_or(0) > 0)
                .and_then(|stream| stream.index)
        })
        .or_else(|| audio.filter_map(|stream| stream.index).next())
}

/// Containers commonly store the human-facing audio name ("English",
/// "Spanish") but leave the ISO language tag as `und`. Promote the two names
/// involved in #278 to proper HLS language codes so clients can both display
/// and select them. A real language tag always wins.
fn audio_stream_language(tags: &FfprobeTags) -> Option<String> {
    let language = tags.language.as_deref().map(str::trim).filter(|value| {
        !value.is_empty()
            && !matches!(
                value.to_ascii_lowercase().as_str(),
                "und" | "undefined" | "undetermined" | "unknown"
            )
    });
    if let Some(language) = language {
        return Some(language.to_string());
    }
    let names = [
        tags.title.as_deref(),
        tags.name.as_deref(),
        tags.handler_name.as_deref(),
    ];
    if names
        .iter()
        .flatten()
        .any(|name| audio_title_has_language(name, "english"))
    {
        Some("eng".to_string())
    } else if names.iter().flatten().any(|name| {
        audio_title_has_language(name, "spanish") || audio_title_has_language(name, "español")
    }) {
        Some("spa".to_string())
    } else {
        None
    }
}

fn audio_title_has_language(title: &str, language: &str) -> bool {
    title
        .split(|character: char| !character.is_alphabetic())
        .any(|word| word.eq_ignore_ascii_case(language))
}

pub(crate) fn is_english(language: &str) -> bool {
    let normalized = language.trim().to_ascii_lowercase();
    let base = normalized.split(['-', '_']).next().unwrap_or(&normalized);
    matches!(base, "en" | "eng" | "english")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn streams(json: &str) -> Vec<FfprobeStream> {
        serde_json::from_str::<FfprobeOutput>(json).unwrap().streams
    }

    #[test]
    fn english_audio_wins_over_an_earlier_default_track() {
        let parsed = streams(
            r#"{"streams":[
                {"index":1,"codec_type":"audio","tags":{"language":"jpn"},"disposition":{"default":1}},
                {"index":2,"codec_type":"audio","tags":{"language":"eng"},"disposition":{"default":0}}
            ]}"#,
        );
        assert_eq!(select_preferred_audio_stream(&parsed), Some(2));
    }

    #[test]
    fn default_then_first_are_safe_fallbacks_when_english_is_absent() {
        let with_default = streams(
            r#"{"streams":[
                {"index":1,"codec_type":"audio","tags":{"language":"jpn"},"disposition":{"default":0}},
                {"index":3,"codec_type":"audio","tags":{"language":"fra"},"disposition":{"default":1}}
            ]}"#,
        );
        let without_default = streams(
            r#"{"streams":[
                {"index":4,"codec_type":"audio"},
                {"index":5,"codec_type":"audio","tags":{"language":"deu"}}
            ]}"#,
        );
        assert_eq!(select_preferred_audio_stream(&with_default), Some(3));
        assert_eq!(select_preferred_audio_stream(&without_default), Some(4));
    }

    #[test]
    fn recognizes_common_english_language_tags() {
        for language in ["en", "eng", "English", "en-US", "en_GB"] {
            assert!(is_english(language), "did not recognize {language}");
        }
        assert!(!is_english("spa"));
    }

    #[test]
    fn infers_english_and_spanish_from_titles_when_language_is_und() {
        let parsed = streams(
            r#"{"streams":[
                {"index":1,"codec_type":"audio","tags":{"language":"und","title":"Spanish Stereo"},"disposition":{"default":1}},
                {"index":2,"codec_type":"audio","tags":{"language":"und","title":"English 5.1"},"disposition":{"default":0}}
            ]}"#,
        );
        assert_eq!(
            audio_stream_language(&parsed[0].tags).as_deref(),
            Some("spa")
        );
        assert_eq!(
            audio_stream_language(&parsed[1].tags).as_deref(),
            Some("eng")
        );
        assert_eq!(select_preferred_audio_stream(&parsed), Some(2));
    }

    #[test]
    fn bit_depth_reads_the_digits_in_the_pixel_format() {
        assert_eq!(bit_depth_from_pix_fmt("yuv420p"), Some(8));
        assert_eq!(bit_depth_from_pix_fmt("yuv420p10le"), Some(10));
        assert_eq!(bit_depth_from_pix_fmt("p010le"), Some(10));
        assert_eq!(bit_depth_from_pix_fmt("yuv444p12le"), Some(12));
        assert_eq!(bit_depth_from_pix_fmt(""), None);
    }

    #[test]
    fn hdr_transfer_matches_pq_and_hlg_only() {
        assert!(is_hdr_transfer("smpte2084"));
        assert!(is_hdr_transfer("arib-std-b67"));
        assert!(is_hdr_transfer("SMPTE2084"));
        assert!(!is_hdr_transfer("bt709"));
        assert!(!is_hdr_transfer(""));
    }

    #[cfg(unix)]
    fn make_executable(path: &Path) {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = std::fs::metadata(path).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(path, permissions).unwrap();
    }

    #[cfg(not(unix))]
    fn make_executable(_path: &Path) {}

    #[test]
    fn configured_ffprobe_path_is_authoritative() {
        let configured = PathBuf::from("/custom/tools/ffprobe");
        let resolved = resolve_ffprobe_path_from(
            Some(configured.clone().into_os_string()),
            None,
            &[PathBuf::from("/another/ffprobe")],
        );
        assert_eq!(resolved, configured);
    }

    #[test]
    fn finds_ffprobe_in_platform_locations_when_path_does_not_contain_it() {
        let directory = std::env::temp_dir().join(format!(
            "swarm-probe-resolve-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let ffprobe = directory.join(if cfg!(windows) {
            "ffprobe.exe"
        } else {
            "ffprobe"
        });
        std::fs::write(&ffprobe, b"test executable").unwrap();
        make_executable(&ffprobe);

        let resolved = resolve_ffprobe_path_from(None, None, std::slice::from_ref(&ffprobe));
        assert_eq!(resolved, ffprobe);

        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn falls_back_to_a_bare_name_when_nothing_is_found() {
        let resolved = resolve_ffprobe_path_from(None, None, &[]);
        let expected = if cfg!(windows) {
            "ffprobe.exe"
        } else {
            "ffprobe"
        };
        assert_eq!(resolved, PathBuf::from(expected));
    }

    #[test]
    fn ffprobe_is_taken_from_beside_an_explicitly_located_ffmpeg() {
        let sibling = ffprobe_path_for(Path::new("/opt/homebrew/bin/ffmpeg"));
        assert_eq!(sibling, PathBuf::from("/opt/homebrew/bin/ffprobe"));
    }

    #[test]
    fn a_numeric_profile_does_not_break_stream_parsing() {
        let parsed = streams(
            r#"{"streams":[
                {"index":0,"codec_type":"video","codec_name":"h264","profile":578,"pix_fmt":"yuv420p10le","color_transfer":"smpte2084"}
            ]}"#,
        );
        assert_eq!(parsed[0].profile.as_deref(), Some("578"));
        assert_eq!(parsed[0].pix_fmt.as_deref(), Some("yuv420p10le"));
        assert_eq!(parsed[0].color_transfer.as_deref(), Some("smpte2084"));
    }
}
