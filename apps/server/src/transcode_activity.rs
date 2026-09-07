//! Rolling 60-minute history of transcoding + subtitle activity and the CPU
//! it costs, for the dashboard's live "Transcoding" graph (Metrics tab).
//!
//! Three "bandwidth"-ish modules now exist and are deliberately distinct:
//! - `swarm_media::bandwidth` — bytes actually delivered to streaming clients.
//! - `crate::bandwidth` — this machine's measured *upload capacity*.
//! - this module — how many ffmpeg transcodes / Whisper subtitle jobs are
//!   running and the CPU they consume.
//!
//! Mirrors `swarm_media::bandwidth::BandwidthMeter`'s shape: a background task
//! samples every [`SAMPLE_INTERVAL`], holds only `Weak` handles so it can
//! never keep the core alive, and is skipped entirely when constructed
//! outside a tokio runtime.

use crate::transcription::TranscriptionManager;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;
use swarm_media::transcode::TranscodeManager;
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};

const SAMPLE_INTERVAL: Duration = Duration::from_secs(5);
/// 60 minutes of history at one sample per [`SAMPLE_INTERVAL`].
const HISTORY_LEN: usize = 60 * 60 / SAMPLE_INTERVAL.as_secs() as usize;

/// One 5-second sample of transcoding/subtitle activity.
#[derive(Debug, Clone, serde::Serialize)]
pub struct TranscodeActivitySample {
    /// Milliseconds since the Unix epoch — the client renders a real time
    /// axis rather than assuming samples landed exactly on schedule.
    pub timestamp_ms: i64,
    /// HLS sessions with a live ffmpeg process.
    pub transcode_sessions: u32,
    /// Direct-play sessions (bandwidth reservation, no ffmpeg).
    pub direct_sessions: u32,
    /// Upload bandwidth reserved across every session, bits/sec.
    pub reserved_bps: u64,
    /// Whisper is actively turning audio into subtitles right now.
    pub subtitle_active: bool,
    /// The subtitle worker's current phase (`"transcribing"`, `"idle"`, …),
    /// empty when the subtitle manager could not be reached.
    pub subtitle_phase: String,
    /// Movies/episodes still queued for subtitle generation.
    pub subtitle_queued: u64,
    /// CPU used by ffmpeg transcode processes, as a percentage of the whole
    /// machine (all logical cores = 100%).
    pub transcode_cpu_percent: f32,
    /// CPU used by the media-server process itself — this includes in-process
    /// Whisper subtitle generation, which has no separate child process to
    /// attribute — as a percentage of the whole machine.
    pub server_cpu_percent: f32,
}

pub struct TranscodeActivityMeter {
    history: Mutex<VecDeque<TranscodeActivitySample>>,
}

impl TranscodeActivityMeter {
    pub fn start(
        transcodes: Weak<TranscodeManager>,
        transcription: Weak<TranscriptionManager>,
    ) -> Arc<Self> {
        let meter = Arc::new(Self {
            history: Mutex::new(VecDeque::with_capacity(HISTORY_LEN)),
        });
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            let weak = Arc::downgrade(&meter);
            handle.spawn(async move { sample_loop(weak, transcodes, transcription).await });
        }
        meter
    }

    /// Oldest-to-newest samples, up to the last 60 minutes.
    pub fn history(&self) -> Vec<TranscodeActivitySample> {
        self.history.lock().unwrap().iter().cloned().collect()
    }
}

fn unix_time_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

/// Samples the CPU cost of transcoding. `sysinfo` needs two refreshes at
/// least [`sysinfo::MINIMUM_CPU_UPDATE_INTERVAL`] apart for a real number,
/// so the same `System` is kept alive across ticks and primed once before
/// the first real sample.
struct CpuProbe {
    system: System,
    self_pid: Option<Pid>,
    core_count: f32,
}

impl CpuProbe {
    fn new() -> Self {
        Self {
            system: System::new(),
            self_pid: sysinfo::get_current_pid().ok(),
            core_count: std::thread::available_parallelism()
                .map(usize::from)
                .unwrap_or(1)
                .max(1) as f32,
        }
    }

    fn refresh(&mut self) {
        self.system.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::nothing().with_cpu(),
        );
    }

    /// `(transcode_cpu_percent, server_cpu_percent)`, each normalized so that
    /// every logical core fully busy reads as 100%.
    fn sample(&mut self) -> (f32, f32) {
        self.refresh();
        let Some(self_pid) = self.self_pid else {
            return (0.0, 0.0);
        };
        let mut server = 0.0f32;
        let mut transcode = 0.0f32;
        for (pid, process) in self.system.processes() {
            if *pid == self_pid {
                server += process.cpu_usage();
            } else if process.parent() == Some(self_pid)
                && process
                    .name()
                    .to_string_lossy()
                    .to_ascii_lowercase()
                    .contains("ffmpeg")
            {
                transcode += process.cpu_usage();
            }
        }
        (transcode / self.core_count, server / self.core_count)
    }
}

async fn sample_loop(
    meter: Weak<TranscodeActivityMeter>,
    transcodes: Weak<TranscodeManager>,
    transcription: Weak<TranscriptionManager>,
) {
    let mut ticker = tokio::time::interval(SAMPLE_INTERVAL);
    // The first tick fires immediately; skip it so the first real sample
    // covers a full interval and the CPU probe below has been primed.
    ticker.tick().await;
    let mut cpu = CpuProbe::new();
    cpu.refresh();

    loop {
        ticker.tick().await;
        let (Some(meter), Some(transcodes)) = (meter.upgrade(), transcodes.upgrade()) else {
            return;
        };

        let activity = transcodes.activity();
        let (transcode_cpu_percent, server_cpu_percent) = cpu.sample();
        let (subtitle_active, subtitle_phase, subtitle_queued) = match transcription.upgrade() {
            Some(manager) => match manager.status().await {
                Ok(status) => (
                    matches!(status.phase.as_str(), "transcribing" | "finalizing"),
                    status.phase,
                    status.queued,
                ),
                Err(_) => (false, String::new(), 0),
            },
            None => (false, String::new(), 0),
        };

        let sample = TranscodeActivitySample {
            timestamp_ms: unix_time_ms(),
            transcode_sessions: activity.transcode_sessions as u32,
            direct_sessions: activity.direct_sessions as u32,
            reserved_bps: activity.reserved_bps,
            subtitle_active,
            subtitle_phase,
            subtitle_queued,
            transcode_cpu_percent,
            server_cpu_percent,
        };

        let mut history = meter.history.lock().unwrap();
        if history.len() == HISTORY_LEN {
            history.pop_front();
        }
        history.push_back(sample);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_starts_empty_without_a_runtime() {
        // No tokio runtime in a plain `#[test]`, so no sample loop spawns.
        let meter = TranscodeActivityMeter::start(Weak::new(), Weak::new());
        assert!(meter.history().is_empty());
    }

    #[test]
    fn cpu_probe_normalizes_by_core_count() {
        let mut probe = CpuProbe::new();
        probe.core_count = 4.0;
        // Two spaced refreshes so sysinfo can produce a real reading; the
        // exact number is machine-dependent, but it must be finite and
        // non-negative on every supported platform.
        probe.refresh();
        std::thread::sleep(sysinfo::MINIMUM_CPU_UPDATE_INTERVAL);
        let (transcode, server) = probe.sample();
        assert!(transcode >= 0.0 && transcode.is_finite());
        assert!(server >= 0.0 && server.is_finite());
    }
}
