//! Real-time streaming bandwidth instrumentation: accumulates bytes actually
//! written to clients and samples the rate every 5 seconds, retaining a
//! rolling 60-minute history for the dashboard's live graph (see
//! `apps/server`'s Metrics tab).
//!
//! Distinct from `apps/server`'s `bandwidth` module, which probes this
//! machine's *upload capacity* against a public speed-test endpoint — this
//! one measures bytes actually delivered to real clients during real
//! playback.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

const SAMPLE_INTERVAL: Duration = Duration::from_secs(5);
/// 60 minutes of history at one sample per `SAMPLE_INTERVAL`.
const HISTORY_LEN: usize = 60 * 60 / SAMPLE_INTERVAL.as_secs() as usize;

#[derive(Debug, Clone, Copy, serde::Serialize)]
pub struct BandwidthSample {
    /// Milliseconds since the Unix epoch, so the client can render a real
    /// time axis rather than assuming samples landed exactly on schedule.
    pub timestamp_ms: i64,
    pub bps: u64,
}

fn unix_time_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

/// Shared meter: any number of concurrent streams call [`record`](Self::record)
/// as bytes leave the server; a background task drains the counter into a
/// bucketed history every [`SAMPLE_INTERVAL`].
pub struct BandwidthMeter {
    bytes_since_sample: AtomicU64,
    history: Mutex<VecDeque<BandwidthSample>>,
}

impl BandwidthMeter {
    pub fn new() -> Arc<Self> {
        let meter = Arc::new(Self {
            bytes_since_sample: AtomicU64::new(0),
            history: Mutex::new(VecDeque::with_capacity(HISTORY_LEN)),
        });
        // Mirrors `TranscodeManager::new`'s background-loop pattern: skipped
        // entirely outside a tokio runtime (e.g. plain unit tests that build
        // a meter without one), and holds only a `Weak` so the loop can never
        // keep the meter alive past its last real owner.
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            let weak = Arc::downgrade(&meter);
            handle.spawn(async move { sample_loop(weak).await });
        }
        meter
    }

    /// Records bytes actually written to a client stream. Call after any
    /// rate-limiter pacing, so this reflects real delivered throughput, not
    /// bytes merely queued to send.
    pub fn record(&self, bytes: u64) {
        if bytes == 0 {
            return;
        }
        self.bytes_since_sample.fetch_add(bytes, Ordering::Relaxed);
    }

    /// Oldest-to-newest samples, up to the last 60 minutes.
    pub fn history(&self) -> Vec<BandwidthSample> {
        self.history.lock().unwrap().iter().copied().collect()
    }

    /// The most recently completed 5-second sample's rate, or 0 before the
    /// first sample lands.
    pub fn current_bps(&self) -> u64 {
        self.history
            .lock()
            .unwrap()
            .back()
            .map(|sample| sample.bps)
            .unwrap_or(0)
    }
}

async fn sample_loop(meter: Weak<BandwidthMeter>) {
    let mut ticker = tokio::time::interval(SAMPLE_INTERVAL);
    // The first tick fires immediately; skipping it means the first real
    // sample still covers one full interval instead of ~0 seconds.
    ticker.tick().await;
    loop {
        ticker.tick().await;
        let Some(meter) = meter.upgrade() else {
            return;
        };
        let bytes = meter.bytes_since_sample.swap(0, Ordering::Relaxed);
        let bps = bytes * 8 / SAMPLE_INTERVAL.as_secs();
        let sample = BandwidthSample {
            timestamp_ms: unix_time_ms(),
            bps,
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
    fn records_accumulate_until_sampled() {
        let meter = BandwidthMeter::new();
        meter.record(1000);
        meter.record(2000);
        // No tokio runtime in this plain `#[test]`, so no sample loop is
        // running yet — the raw counter should reflect both writes.
        assert_eq!(meter.bytes_since_sample.load(Ordering::Relaxed), 3000);
        assert_eq!(meter.current_bps(), 0);
        assert!(meter.history().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn sample_loop_buckets_every_five_seconds() {
        let meter = BandwidthMeter::new();
        // Lets the freshly spawned sample loop run once and register its
        // interval before we advance virtual time out from under it.
        tokio::task::yield_now().await;
        meter.record(625_000); // 5,000,000 bits over 5s == 1,000,000 bps
        tokio::time::advance(SAMPLE_INTERVAL).await;
        tokio::time::advance(Duration::from_millis(10)).await;
        assert_eq!(meter.current_bps(), 1_000_000);
        assert_eq!(meter.history().len(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn history_caps_at_sixty_minutes() {
        let meter = BandwidthMeter::new();
        for _ in 0..HISTORY_LEN + 5 {
            meter.record(1);
            tokio::time::advance(SAMPLE_INTERVAL).await;
        }
        tokio::time::advance(Duration::from_millis(10)).await;
        assert_eq!(meter.history().len(), HISTORY_LEN);
    }
}
