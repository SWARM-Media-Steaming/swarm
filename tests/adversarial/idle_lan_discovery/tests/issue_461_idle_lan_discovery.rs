//! Issue #461: after the media server sat unused for a week (still running
//! on the machine, same LAN, no config change) the TV listed it as offline.
//! Restarting the media server made the TV see it again.
//!
//! Expected behavior, derived from the issue, PROTOCOL.md ("On the same LAN,
//! … the media server advertises itself over mDNS, the TV client lists it
//! automatically"), and DNS-SD/mDNS invariants — not from the patch:
//!
//! 1. A process that is still running must remain detectable on the LAN
//!    indefinitely. The owner must not have to restart it for a TV that
//!    already shares the network to find it again.
//! 2. mDNS host records (A/SRV) expire after 120 seconds (RFC 6762 /
//!    mdns-sd). Unsolicited re-announcement, if that is how the server
//!    keeps a silent TV's cache warm, must happen strictly more often than
//!    that TTL. PTR/TXT default to 75 minutes; the host TTL is the tighter
//!    bound a TV that dropped the record actually feels.
//! 3. The only loop that can re-announce is the connectivity supervisor.
//!    Every delay that loop can sleep — including the unused-SWARM dormant
//!    schedule that production logs for this incident were on — must still
//!    give re-announce a chance before host records expire.
//! 4. Loopback-only test/dev listeners must still not advertise, so a
//!    dying test does not leave a phantom "SWARM Media Server" on the LAN.
//! 5. Re-announcing an unchanged network must keep the same instance name,
//!    fingerprint, and ports so the TV treats it as the same server coming
//!    back, not a second server.
//!
//! Production evidence for (3), 2026-10-09 ~20:12 CDT: the GUI process was
//! alive (hourly bandwidth samples, SWARM retries at attempts=1330) until
//! `logging initialized` at 2026-10-10T01:10:42Z, then immediately
//! `advertising media server on the LAN` and a LAN peer from 192.168.0.245.
//! Retries were 10 minutes apart with "nothing is paired through it" —
//! `LinkPolicy::dormant_retry` on an unused unreachable managed link.

use mdns_sd::{ServiceDaemon, ServiceEvent};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use swarm_media::roots::MediaRoot;
use swarm_server::lan::{self, SERVICE_TYPE};
use swarm_server::link::LinkPolicy;
use swarm_server::{ServerConfig, ServerCore, TokenStoreMode};

static FIXTURE: AtomicU64 = AtomicU64::new(0);
static MDNS_LOCK: Mutex<()> = Mutex::new(());

/// RFC 6762 / mdns-sd `DNS_HOST_TTL`: A and SRV records last two minutes.
const MDNS_HOST_TTL: Duration = Duration::from_secs(120);

struct Fixture {
    base: PathBuf,
    core: Option<Arc<ServerCore>>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.core.take();
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

fn test_config(bind: SocketAddr, data_dir: PathBuf, media_root: PathBuf) -> ServerConfig {
    ServerConfig {
        media_roots: vec![MediaRoot {
            label: "local".into(),
            path: media_root,
            asset_type: Default::default(),
        }],
        scan_options: Default::default(),
        data_dir,
        bind,
        http_media_bind: "127.0.0.1:0".parse().unwrap(),
        http_media_tls_bind: None,
        allowed_fingerprints: vec![],
        token_store_mode: TokenStoreMode::FileOnly,
        managed_rendezvous_url: None,
    }
}

async fn start_core(tag: &str, bind: SocketAddr) -> Fixture {
    let n = FIXTURE.fetch_add(1, Ordering::Relaxed);
    let base = std::env::temp_dir().join(format!(
        "swarm-adv-461-{tag}-{}-{n}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&base);
    let media_root = base.join("media");
    std::fs::create_dir_all(&media_root).unwrap();
    let data_dir = base.join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let core = ServerCore::start(test_config(bind, data_dir, media_root))
        .await
        .unwrap();
    Fixture {
        base,
        core: Some(core),
    }
}

fn browse_resolved(fingerprint: &str, timeout: Duration) -> Option<mdns_sd::ResolvedService> {
    let daemon = ServiceDaemon::new().expect("mDNS browser daemon");
    let rx = daemon.browse(SERVICE_TYPE).expect("browse _swarm-peer._udp");
    let deadline = Instant::now() + timeout;
    let mut found = None;
    while Instant::now() < deadline {
        let wait = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(wait.min(Duration::from_millis(250))) {
            Ok(ServiceEvent::ServiceResolved(info)) => {
                if info.get_property_val_str("fingerprint") == Some(fingerprint) {
                    found = Some(*info);
                    break;
                }
            }
            Ok(_) => {}
            Err(_) => {}
        }
    }
    let _ = daemon.shutdown();
    found
}

fn repo_file(relative: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .join(relative);
    std::fs::read_to_string(&path).unwrap_or_else(|err| panic!("read {}: {err}", path.display()))
}

/// The advertised host-record lifetime is two minutes. Anything that is
/// supposed to keep a silent TV from going "offline" has to beat that.
#[test]
fn reannounce_interval_is_strictly_inside_mdns_host_ttl() {
    assert!(
        lan::REANNOUNCE_INTERVAL > Duration::ZERO,
        "REANNOUNCE_INTERVAL must actually fire"
    );
    assert!(
        lan::REANNOUNCE_INTERVAL < MDNS_HOST_TTL,
        "REANNOUNCE_INTERVAL {:?} is not shorter than the 120s mDNS host TTL; a TV that dropped the A/SRV record would still go offline between announcements",
        lan::REANNOUNCE_INTERVAL
    );
}

/// Production #461 was not a healthy 15s health-check loop. The unused
/// managed SWARM link had been unreachable for days, so its retry policy was
/// dormant (10 minutes). That SWARM retry must stay slow, while a separate
/// supervisor wake still services the LAN reannounce deadline.
#[test]
fn supervisor_delay_cannot_starve_reannounce_while_swarm_is_dormant() {
    let policy = LinkPolicy::default();
    assert!(
        policy.health_interval <= lan::REANNOUNCE_INTERVAL,
        "healthy/LAN-only supervisor tick {:?} is slower than REANNOUNCE_INTERVAL {:?}",
        policy.health_interval,
        lan::REANNOUNCE_INTERVAL
    );

    let dormant_delay = policy.retry_delay_for(u32::MAX, true);
    assert!(
        dormant_delay >= Duration::from_secs(10 * 60),
        "unused SWARM should retain its slow dormant retry policy; got {dormant_delay:?}"
    );

    // The reannounce deadline is an independent wake source. Testing the
    // raw dormant retry against the LAN interval would accidentally require
    // polling an unreachable SWARM service every minute.
    let lib = repo_file("apps/server/src/lib.rs");
    let supervisor = lib
        .split("let mut next_pass = tokio::time::Instant::now();")
        .nth(1)
        .expect("supervisor loop with independent next_pass deadline");
    let wait = supervisor
        .split("let announce_in = match last_announce")
        .nth(1)
        .expect("supervisor computes time to LAN reannounce");
    let wait = wait
        .split("tokio::select!")
        .next()
        .expect("supervisor computes its sleep before selecting wake sources");
    assert!(
        wait.contains("next_pass")
            && wait.contains("saturating_duration_since")
            && wait.contains("min(announce_in)"),
        "supervisor sleep must be the earlier of the SWARM pass and LAN reannounce deadline"
    );
    assert!(
        wait.contains("REANNOUNCE_INTERVAL.saturating_sub(last.elapsed())"),
        "LAN wake deadline must be based on elapsed time since last successful announcement"
    );
}

/// The supervisor must re-announce on an *unchanged* network. Refresh-on-
/// address-change is a different bug (stale DHCP) and does not cover a week
/// of sitting still on the same LAN.
#[test]
fn supervisor_reannounces_without_waiting_for_a_network_change() {
    let lib = repo_file("apps/server/src/lib.rs");
    let lan = repo_file("apps/server/src/lan.rs");

    assert!(
        lib.contains("self.lan_service.reannounce()"),
        "supervise_once must call lan_service.reannounce so an idle unchanged network still announces"
    );
    assert!(
        lib.contains("last_announce") && lib.contains("REANNOUNCE_INTERVAL"),
        "the supervisor must track last_announce against REANNOUNCE_INTERVAL"
    );

    let reannounce_idx = lib
        .find("self.lan_service.reannounce()")
        .expect("reannounce call");
    let observe_idx = lib
        .find("watch.observe(swarm_p2p::local_addr::detect_local_ipv4())")
        .expect("address watch");
    assert!(
        reannounce_idx < observe_idx,
        "periodic reannounce must run even when AddressWatch reports no move; it currently sits after the network-change refresh"
    );

    assert!(
        lan.contains("pub fn reannounce(") && lan.contains("pub fn refresh_advertisement("),
        "network-change refresh and idle reannounce are distinct operations"
    );
    let refresh = lan
        .split("pub fn refresh_advertisement")
        .nth(1)
        .expect("refresh_advertisement body");
    let refresh_body = refresh.split("pub fn reannounce").next().unwrap();
    assert!(
        refresh_body.contains("self.reannounce()"),
        "refresh_advertisement must still re-announce after a real network change by calling reannounce()"
    );
}

#[test]
fn last_announce_must_not_advance_when_reannounce_sends_nothing() {
    let lib = repo_file("apps/server/src/lib.rs");
    let haystack = lib
        .split("if last_announce.is_none_or")
        .nth(1)
        .expect("last_announce gate in supervise_once");
    let gate = haystack.split("watch.observe").next().unwrap();
    assert!(
        gate.contains("reannounce()"),
        "the last_announce gate must actually call reannounce"
    );
    // Stamp the clock only after a successful send. `reannounce` returns
    // None when nothing is advertised or mdns-sd rejects the register
    // (daemon thread gone — the restart-shaped failure in #461). Advancing
    // last_announce first would then wait a full interval before trying
    // again, and a dormant supervisor would wait ten minutes.
    let stamp = gate.find("*last_announce = Some");
    let call = gate.find("self.lan_service.reannounce()");
    match (stamp, call) {
        (Some(stamp), Some(call)) => assert!(
            stamp > call,
            "last_announce is advanced before reannounce() returns; a failed send is then treated as a successful interval"
        ),
        _ => panic!("supervise_once must assign last_announce around the reannounce() call"),
    }
}

/// A running server bound on a real interface is findable by a browser that
/// was not listening at process start — the TV-after-a-week case, without
/// waiting a week. Restarting the server is not required.
#[tokio::test]
async fn running_lan_server_is_found_by_a_late_joining_browser_without_restart() {
    let _guard = MDNS_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let local = swarm_p2p::local_addr::detect_local_ipv4();
    assert!(
        !local.is_loopback() && !local.is_unspecified(),
        "this host has no LAN IPv4 to advertise; #461 cannot be exercised here"
    );

    let fx = start_core("late-join", "0.0.0.0:0".parse().unwrap()).await;
    let core = fx.core.as_ref().unwrap();
    let fingerprint = core.identity.fingerprint.clone();
    let listen = core.listen_addr;

    // Pause long enough that the initial unsolicited burst (RFC 6762: two
    // packets, one second apart) is over. A TV that opens the app later
    // must still resolve the service from a live responder / later
    // announcement, not from having caught startup.
    tokio::time::sleep(Duration::from_secs(2)).await;

    let resolved = browse_resolved(&fingerprint, Duration::from_secs(8)).unwrap_or_else(|| {
        panic!("late-joining browser did not resolve fingerprint {fingerprint} without a server restart")
    });

    assert_eq!(
        resolved.get_property_val_str("fingerprint"),
        Some(fingerprint.as_str())
    );
    assert_eq!(
        resolved.get_property_val_str("name"),
        Some("SWARM Media Server")
    );
    assert_eq!(
        resolved.get_property_val_str("peer_port"),
        Some(listen.port().to_string()).as_deref()
    );
    assert_eq!(resolved.get_port(), listen.port());
    assert!(
        resolved
            .get_addresses_v4()
            .iter()
            .any(|ip| IpAddr::V4(*ip) == local || !ip.is_loopback()),
        "advertised addresses {:?} must be reachable from the LAN, not loopback",
        resolved.get_addresses_v4()
    );
    assert!(
        !resolved.get_fullname().is_empty()
            && resolved.get_fullname().contains(&fingerprint[..12]),
        "instance name must stay tied to the certificate fingerprint so a reannounce replaces the same record"
    );
}

/// Dropping the running server (goodbye / unregister) must make a fresh
/// browser stop resolving it. If this failed, a restart would not be what
/// brought the TV back — leftover cache would.
#[tokio::test]
async fn stopping_the_server_withdraws_the_advertisement() {
    let _guard = MDNS_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let local = swarm_p2p::local_addr::detect_local_ipv4();
    assert!(
        !local.is_loopback() && !local.is_unspecified(),
        "this host has no LAN IPv4 to advertise"
    );

    let mut fx = start_core("withdraw", "0.0.0.0:0".parse().unwrap()).await;
    let fingerprint = fx.core.as_ref().unwrap().identity.fingerprint.clone();
    assert!(
        browse_resolved(&fingerprint, Duration::from_secs(8)).is_some(),
        "precondition: the live server must be discoverable before we stop it"
    );

    fx.core.take();
    tokio::time::sleep(Duration::from_secs(1)).await;

    assert!(
        browse_resolved(&fingerprint, Duration::from_secs(3)).is_none(),
        "after the process is gone the TV must not keep resolving this instance as a live server"
    );
}

/// Loopback servers must not appear on the LAN. The idle-reannounce path
/// must not undo that (a None advertisement stays None).
#[tokio::test]
async fn loopback_listener_is_not_discoverable_on_the_lan() {
    let _guard = MDNS_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let fx = start_core("loopback", "127.0.0.1:0".parse().unwrap()).await;
    let fingerprint = fx.core.as_ref().unwrap().identity.fingerprint.clone();
    assert_eq!(
        fx.core.as_ref().unwrap().listen_addr.ip(),
        IpAddr::V4(Ipv4Addr::LOCALHOST)
    );
    assert!(
        browse_resolved(&fingerprint, Duration::from_secs(3)).is_none(),
        "a loopback-only core must not publish _swarm-peer._udp, even after the idle reannounce supervisor runs"
    );
}
