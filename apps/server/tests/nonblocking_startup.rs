//! A real user hit this: their library was ~11,000 files on a network
//! share, and `ServerCore::start()` used to await the entire initial scan
//! inline before returning. Since every Tauri command builds/reuses this
//! same core through a `OnceCell`, the very first command after launch —
//! and therefore *every* command from *every* tab, since they all await the
//! same in-flight future — blocked on the whole scan. The app was
//! unresponsive (every tab stuck on "Loading…") for as long as the scan
//! took, which for a large network-mounted library is many minutes.
//!
//! Proves `start()` no longer does this: it returns quickly regardless of
//! library size, and a command that doesn't itself touch scanning (like
//! `status()`) stays responsive even while the initial scan is still
//! running in the background.

use std::time::{Duration, Instant};
use swarm_media::roots::MediaRoot;
use swarm_server::{ServerConfig, ServerCore, TokenStoreMode};

fn config(data_dir: std::path::PathBuf, media_root: std::path::PathBuf) -> ServerConfig {
    ServerConfig {
        media_roots: vec![MediaRoot {
            label: "local".to_string(),
            path: media_root,
            asset_type: Default::default(),
        }],
        scan_options: Default::default(),
        data_dir,
        bind: "127.0.0.1:0".parse().unwrap(),
        http_media_bind: "127.0.0.1:0".parse().unwrap(),
        http_media_tls_bind: None,
        allowed_fingerprints: vec![],
        token_store_mode: TokenStoreMode::FileOnly,
        managed_rendezvous_url: None,
    }
}

/// `start()`'s own duration must not scale with library size — previously
/// it did, linearly, because it awaited the full scan inline.
#[tokio::test]
async fn start_returns_quickly_regardless_of_library_size() {
    let base =
        std::env::temp_dir().join(format!("swarm-nonblocking-startup-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);

    let empty_root = base.join("empty");
    std::fs::create_dir_all(&empty_root).unwrap();
    let started = Instant::now();
    let empty_core = ServerCore::start(config(base.join("empty-data"), empty_root))
        .await
        .unwrap();
    let empty_elapsed = started.elapsed();

    let big_root = base.join("big");
    std::fs::create_dir_all(&big_root).unwrap();
    for i in 0..1000 {
        let path = big_root.join(format!("movies/Movie {i}/Movie.{i}.mkv"));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, vec![7u8; 4096]).unwrap();
    }
    let started = Instant::now();
    let big_core = ServerCore::start(config(base.join("big-data"), big_root))
        .await
        .unwrap();
    let big_elapsed = started.elapsed();

    // The invariant is "start() does not scale with library size", not any
    // one absolute number — a shared CI runner is far slower and noisier than
    // a dev laptop, so assert relatively (the 1000-file start is not
    // dramatically slower than the empty one) with only a loose absolute
    // backstop for the "scan awaited inline again" regression this guards.
    assert!(
        empty_elapsed < Duration::from_secs(5),
        "empty-root start() took {empty_elapsed:?} — something other than scanning is blocking"
    );
    assert!(
        big_elapsed < empty_elapsed + Duration::from_millis(750)
            && big_elapsed < Duration::from_secs(5),
        "1000-file start() took {big_elapsed:?} vs {empty_elapsed:?} for an empty root — \
        looks like the initial scan is being awaited inline again"
    );

    // The background scan does still genuinely happen and finish correctly
    // — this isn't "the scan silently never runs", just "it doesn't block
    // startup".
    let report = big_core.wait_for_scan().await.unwrap();
    assert_eq!(report.added, 1000);
    assert_eq!(big_core.library.entry_count().await.unwrap(), 1000);

    let _ = std::fs::remove_dir_all(&base);
    drop(empty_core);
}

/// The real symptom: every OTHER command (not just start() itself) was
/// blocked by one in-flight scan. `status()` never touches scan_lock, so it
/// must stay responsive whether or not a scan happens to still be running —
/// wrapped in a timeout so a regression fails fast instead of hanging.
#[tokio::test]
async fn other_commands_stay_responsive_during_the_initial_scan() {
    let base =
        std::env::temp_dir().join(format!("swarm-nonblocking-commands-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let media_root = base.join("media");
    std::fs::create_dir_all(&media_root).unwrap();
    for i in 0..1000 {
        let path = media_root.join(format!("movies/Movie {i}/Movie.{i}.mkv"));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, vec![7u8; 4096]).unwrap();
    }

    let core = ServerCore::start(config(base.join("data"), media_root))
        .await
        .unwrap();

    // Deliberately not waiting for the scan here — status() must respond
    // regardless of whether it's still in flight. The timeout only has to
    // distinguish "responds" from "hangs on scan_lock", so a CI-safe 2s is
    // plenty — a real regression blocks until the whole 1000-file scan ends.
    let status = tokio::time::timeout(Duration::from_secs(2), core.status())
        .await
        .expect("status() must not block on an in-progress scan")
        .unwrap();
    // Whichever state we happened to catch it in, it must be a real,
    // internally-consistent snapshot, not a hang.
    assert!(status.entry_count <= 1000);

    let report = core.wait_for_scan().await.unwrap();
    assert_eq!(report.added, 1000);

    let _ = std::fs::remove_dir_all(&base);
}
