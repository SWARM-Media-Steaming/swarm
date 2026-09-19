//! Supervision of this server's link to the SWARM rendezvous service, and of
//! the local network address everything else depends on.
//!
//! Why this exists: the link used to be set up exactly once, at startup. If
//! the service was unreachable then (a different network, a service on a
//! since-changed DHCP address, a reboot race) the failure was logged once and
//! never retried, and the server stayed invisible to every SWARM-paired TV
//! until someone restarted it — while every local check still looked healthy.
//! A network change *after* startup was worse: nothing noticed at all.
//!
//! This module holds the pure, unit-testable parts: the status the UI and the
//! health endpoint report, the retry schedule, and change detection for the
//! local address. The async driver lives in `lib.rs` next to the state it
//! mutates.

use serde::Serialize;
use std::net::IpAddr;
use std::time::Duration;

/// Where the link to the SWARM service stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SwarmLinkState {
    /// No SWARM service is configured or saved. A LAN-only server is
    /// perfectly healthy in this state.
    NotLinked,
    /// First connection attempt is in flight.
    Connecting,
    /// Registered with the service and holding a live signaling session, so
    /// SWARM-paired clients can see this server as online.
    Connected,
    /// A service is configured or saved but cannot be reached (or the
    /// session dropped). Retrying with backoff; LAN clients are unaffected.
    Unreachable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SwarmLinkStatus {
    pub state: SwarmLinkState,
    /// The service address in use, or being tried.
    pub base_url: Option<String>,
    /// Why the most recent attempt failed. Cleared once connected.
    pub last_error: Option<String>,
    /// Unix seconds of the first failure in the current outage.
    pub failing_since: Option<u64>,
    /// Unix seconds since the current connection was established.
    pub connected_since: Option<u64>,
    /// Consecutive failed attempts in the current outage.
    pub attempts: u32,
    /// Whether a signaling session is currently open. This is what marks the
    /// server "online" in a swarm roster, so it is reported separately from
    /// `state` for diagnosis.
    pub signaling: bool,
}

impl SwarmLinkStatus {
    pub fn not_linked() -> Self {
        Self {
            state: SwarmLinkState::NotLinked,
            base_url: None,
            last_error: None,
            failing_since: None,
            connected_since: None,
            attempts: 0,
            signaling: false,
        }
    }

    /// The part that is safe to serve to any device on the LAN without
    /// authentication: no service address and no error text.
    pub fn public(&self) -> PublicLinkStatus {
        PublicLinkStatus {
            state: self.state,
            failing_since: self.failing_since,
            attempts: self.attempts,
            signaling: self.signaling,
        }
    }
}

/// See [`SwarmLinkStatus::public`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PublicLinkStatus {
    pub state: SwarmLinkState,
    pub failing_since: Option<u64>,
    pub attempts: u32,
    pub signaling: bool,
}

/// Timing for the supervisor. Environment overrides exist so integration
/// tests can run in milliseconds; they follow the same convention as
/// `SWARM_*` knobs elsewhere in this crate and are not user-facing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LinkPolicy {
    /// Delay before the first retry after a failure.
    pub initial_retry: Duration,
    /// Ceiling for the doubling retry delay.
    pub max_retry: Duration,
    /// How often a healthy link (and the local address) is re-checked.
    pub health_interval: Duration,
    /// Upper bound on one background connection attempt. A black-holed
    /// address (a different network, a powered-off host) otherwise hangs
    /// until the OS gives up, which is minutes.
    pub attempt_timeout: Duration,
    /// How long `ServerCore::start` waits for the first attempt before
    /// letting the supervisor finish it in the background. Every GUI command
    /// waits on the core, so an unreachable service must never hold startup
    /// hostage.
    pub startup_wait: Duration,
}

impl Default for LinkPolicy {
    fn default() -> Self {
        Self {
            initial_retry: Duration::from_secs(5),
            // A request every minute to a service that is down costs nothing,
            // and a low ceiling means recovery after the service or network
            // returns is noticed within a minute rather than several.
            max_retry: Duration::from_secs(60),
            health_interval: Duration::from_secs(15),
            attempt_timeout: Duration::from_secs(20),
            startup_wait: Duration::from_secs(5),
        }
    }
}

impl LinkPolicy {
    pub fn from_env() -> Self {
        let default = Self::default();
        let millis = |name: &str, fallback: Duration| {
            std::env::var(name)
                .ok()
                .and_then(|value| value.trim().parse::<u64>().ok())
                .filter(|value| *value > 0)
                .map(Duration::from_millis)
                .unwrap_or(fallback)
        };
        Self {
            initial_retry: millis("SWARM_LINK_RETRY_INITIAL_MS", default.initial_retry),
            max_retry: millis("SWARM_LINK_RETRY_MAX_MS", default.max_retry),
            health_interval: millis("SWARM_LINK_CHECK_MS", default.health_interval),
            attempt_timeout: millis("SWARM_LINK_ATTEMPT_TIMEOUT_MS", default.attempt_timeout),
            startup_wait: millis("SWARM_LINK_STARTUP_WAIT_MS", default.startup_wait),
        }
    }

    /// Delay before retry number `failures` (1 for the first retry):
    /// `initial_retry`, doubling each time, capped at `max_retry`.
    pub fn retry_delay(&self, failures: u32) -> Duration {
        let doublings = failures.saturating_sub(1).min(16);
        self.initial_retry
            .saturating_mul(1u32 << doublings)
            .min(self.max_retry)
    }
}

/// Notices when this machine's LAN address changes.
///
/// Loopback and unspecified addresses mean "no usable network right now"
/// (`detect_local_ipv4` falls back to loopback when offline). They are never
/// recorded and never reported as a change, so unplugging the cable does not
/// make the server advertise `127.0.0.1`.
#[derive(Debug)]
pub struct AddressWatch {
    last: Option<IpAddr>,
}

impl AddressWatch {
    pub fn new(initial: IpAddr) -> Self {
        Self {
            last: Self::usable(initial).then_some(initial),
        }
    }

    fn usable(ip: IpAddr) -> bool {
        !ip.is_loopback() && !ip.is_unspecified()
    }

    /// Records `now` and returns the new address if it differs from the last
    /// usable one.
    pub fn observe(&mut self, now: IpAddr) -> Option<IpAddr> {
        if !Self::usable(now) {
            return None;
        }
        match self.last.replace(now) {
            Some(previous) if previous == now => None,
            // First usable address after starting offline: nothing was
            // advertised on a stale address, but nothing valid was advertised
            // at all, so it still counts as a change worth acting on.
            _ => Some(now),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr};

    fn ip(a: u8, b: u8, c: u8, d: u8) -> IpAddr {
        IpAddr::V4(Ipv4Addr::new(a, b, c, d))
    }

    #[test]
    fn retry_delay_doubles_from_the_initial_delay_and_stops_at_the_cap() {
        let policy = LinkPolicy::default();
        let secs = |n| Duration::from_secs(n);
        assert_eq!(policy.retry_delay(1), secs(5));
        assert_eq!(policy.retry_delay(2), secs(10));
        assert_eq!(policy.retry_delay(3), secs(20));
        assert_eq!(policy.retry_delay(4), secs(40));
        assert_eq!(policy.retry_delay(5), secs(60));
        assert_eq!(policy.retry_delay(6), secs(60));
        // A very long outage must neither overflow nor exceed the cap.
        assert_eq!(policy.retry_delay(u32::MAX), secs(60));
    }

    #[test]
    fn retry_delay_treats_zero_failures_like_the_first_retry() {
        assert_eq!(LinkPolicy::default().retry_delay(0), Duration::from_secs(5));
    }

    #[test]
    fn address_watch_reports_a_move_between_networks_once() {
        let mut watch = AddressWatch::new(ip(192, 168, 0, 133));
        assert_eq!(watch.observe(ip(192, 168, 0, 133)), None);
        assert_eq!(watch.observe(ip(10, 0, 0, 7)), Some(ip(10, 0, 0, 7)));
        // Same address on the next poll is not a second change.
        assert_eq!(watch.observe(ip(10, 0, 0, 7)), None);
        // And moving back is a change again.
        assert_eq!(
            watch.observe(ip(192, 168, 0, 133)),
            Some(ip(192, 168, 0, 133))
        );
    }

    #[test]
    fn address_watch_ignores_loopback_and_unspecified_while_offline() {
        let mut watch = AddressWatch::new(ip(192, 168, 0, 133));
        assert_eq!(watch.observe(IpAddr::V4(Ipv4Addr::LOCALHOST)), None);
        assert_eq!(watch.observe(IpAddr::V4(Ipv4Addr::UNSPECIFIED)), None);
        assert_eq!(watch.observe(IpAddr::V6(Ipv6Addr::LOCALHOST)), None);
        // Coming back on the *same* address after a brief offline blip is not
        // a change: the advertisement on it is still correct.
        assert_eq!(watch.observe(ip(192, 168, 0, 133)), None);
    }

    #[test]
    fn address_watch_started_offline_reports_the_first_real_address() {
        let mut watch = AddressWatch::new(IpAddr::V4(Ipv4Addr::LOCALHOST));
        assert_eq!(watch.observe(ip(192, 168, 0, 133)), Some(ip(192, 168, 0, 133)));
        assert_eq!(watch.observe(ip(192, 168, 0, 133)), None);
    }

    #[test]
    fn public_status_never_exposes_the_service_address_or_error_text() {
        let status = SwarmLinkStatus {
            state: SwarmLinkState::Unreachable,
            base_url: Some("http://192.168.0.235:8080".into()),
            last_error: Some("connection refused".into()),
            failing_since: Some(100),
            connected_since: None,
            attempts: 3,
            signaling: false,
        };
        let json = serde_json::to_string(&status.public()).unwrap();
        assert!(!json.contains("192.168"), "leaked address: {json}");
        assert!(!json.contains("refused"), "leaked error: {json}");
        assert!(json.contains("\"state\":\"unreachable\""), "{json}");
        assert!(json.contains("\"attempts\":3"), "{json}");
    }
}
