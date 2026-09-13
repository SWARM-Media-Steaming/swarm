//! A real user hit this: remove a media root, add a replacement, and the
//! running server kept serving the old one until a manual restart —
//! `ServerCore.media_roots`/`MediaService.roots` were each a private
//! snapshot, taken once at `start()` and never updated again.
//!
//! Proves `ServerCore::update_media_roots` fixes that for real: swapping a
//! running core's roots (no restart, no new `ServerCore`) reconciles the
//! library against the new set, and — critically — a real QUIC connection
//! served by the SAME core's `MediaService` sees the new root too, proving
//! the two don't drift onto different root sets after a live change.

use swarm_core::peer::{CatalogManifest, MediaKind, PeerRequest};
use swarm_media::roots::MediaRoot;
use swarm_p2p::endpoint::{connect, read_body, send_request};
use swarm_p2p::identity::ensure_identity;
use swarm_server::{ServerConfig, ServerCore, TokenStoreMode};

fn deterministic_bytes(len: usize, seed: u8) -> Vec<u8> {
    (0..len)
        .map(|i| seed.wrapping_add((i * 31 + i / 251) as u8))
        .collect()
}

fn no_range(path: &str) -> PeerRequest {
    PeerRequest {
        path: path.into(),
        range: None,
        if_none_match: None,
        playback: None,
        error_report: None,
        like: None,
    }
}

#[tokio::test]
async fn update_media_roots_takes_effect_live_for_both_scanning_and_serving() {
    let base = std::env::temp_dir().join(format!("swarm-live-roots-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);

    // Root A: what the server starts with — mirrors the user's real
    // scenario ("local" folder that existed before this change).
    let root_a = base.join("root-a");
    let file_a = root_a.join("movies/Old Movie (2020)/Old.Movie.2020.mkv");
    std::fs::create_dir_all(file_a.parent().unwrap()).unwrap();
    let bytes_a = deterministic_bytes(50_000, 1);
    std::fs::write(&file_a, &bytes_a).unwrap();

    // Root B: what the user swaps to live — mirrors a newly mounted NAS
    // share added after removing the old root, never known to this core at
    // start().
    let root_b = base.join("root-b");
    let file_b = root_b.join("movies/New Movie (2024)/New.Movie.2024.mkv");
    std::fs::create_dir_all(file_b.parent().unwrap()).unwrap();
    let bytes_b = deterministic_bytes(70_000, 2);
    std::fs::write(&file_b, &bytes_b).unwrap();

    let client_identity = ensure_identity(&base.join("client-id")).unwrap();

    let config = ServerConfig {
        media_roots: vec![MediaRoot {
            label: "local".into(),
            path: root_a.clone(),
            asset_type: Default::default(),
        }],
        scan_options: Default::default(),
        data_dir: base.join("server-data"),
        bind: "127.0.0.1:0".parse().unwrap(),
        http_media_bind: "127.0.0.1:0".parse().unwrap(),
        http_media_tls_bind: None,
        allowed_fingerprints: vec![client_identity.fingerprint.clone()],
        token_store_mode: TokenStoreMode::FileOnly,
        managed_rendezvous_url: None,
    };
    let core = ServerCore::start(config).await.unwrap();
    // The initial scan now runs in the background (see ServerCore::start's
    // doc comment) — wait for it explicitly so the assertions below are
    // deterministic instead of racing a real filesystem walk.
    let start_report = core.wait_for_scan().await.unwrap();
    assert_eq!(
        start_report.added, 1,
        "root A's one file should be scanned at start"
    );
    assert_eq!(core.library.entry_count().await.unwrap(), 1);

    let connection = connect(
        core.listen_addr,
        &client_identity,
        &core.identity.fingerprint,
    )
    .await
    .unwrap();

    // Sanity before the swap: root A's file is really being served over
    // real QUIC by this exact core's MediaService.
    let (header, mut recv) = send_request(&connection, &no_range("/catalog/manifest"))
        .await
        .unwrap();
    assert_eq!(header.status, 200);
    let manifest: CatalogManifest =
        serde_json::from_slice(&read_body(&header, &mut recv).await.unwrap()).unwrap();
    assert_eq!(manifest.entries.len(), 1);
    let entry_a = manifest.entries[0].clone();
    let (header, mut recv) = send_request(
        &connection,
        &no_range(&format!("/media/{}", entry_a.entry_key)),
    )
    .await
    .unwrap();
    assert_eq!(header.status, 200);
    assert_eq!(read_body(&header, &mut recv).await.unwrap(), bytes_a);

    // The live swap — no restart, same running core, same open QUIC
    // connection reused below.
    let update_report = core
        .update_media_roots(vec![MediaRoot {
            label: "local".into(),
            path: root_b.clone(),
            asset_type: Default::default(),
        }])
        .await
        .unwrap();
    assert_eq!(update_report.added, 1, "root B's file must be discovered");
    assert_eq!(
        update_report.removed, 1,
        "root A's file must be reconciled away, same as any other deleted file"
    );

    // ServerCore's own view (library/scan side) reflects only root B now.
    assert_eq!(core.library.entry_count().await.unwrap(), 1);
    assert!(
        core.library
            .get(&entry_a.entry_key)
            .await
            .unwrap()
            .is_none(),
        "root A's entry must be gone"
    );

    // The real proof: MediaService — a SEPARATE struct holding its own
    // clone of the resolver handle — must see the same swap, over the SAME
    // already-open QUIC connection, with no reconnect. If ServerCore and
    // MediaService had drifted onto different resolvers (the bug this test
    // exists to catch), this would 404 even though the library says the
    // entry exists.
    let (header, mut recv) = send_request(&connection, &no_range("/catalog/manifest"))
        .await
        .unwrap();
    assert_eq!(header.status, 200);
    let manifest: CatalogManifest =
        serde_json::from_slice(&read_body(&header, &mut recv).await.unwrap()).unwrap();
    assert_eq!(manifest.entries.len(), 1);
    let entry_b = &manifest.entries[0];
    assert_eq!(entry_b.kind, MediaKind::Movie);
    assert_ne!(
        entry_b.entry_key, entry_a.entry_key,
        "different file, must be a different entry_key"
    );

    let (header, mut recv) = send_request(
        &connection,
        &no_range(&format!("/media/{}", entry_b.entry_key)),
    )
    .await
    .unwrap();
    assert_eq!(
        header.status, 200,
        "MediaService must resolve root B's file live, no restart"
    );
    assert_eq!(read_body(&header, &mut recv).await.unwrap(), bytes_b);

    // The old root's file must be genuinely unreachable now, not just
    // absent from the manifest.
    let (header, _) = send_request(
        &connection,
        &no_range(&format!("/media/{}", entry_a.entry_key)),
    )
    .await
    .unwrap();
    assert_eq!(header.status, 404);

    // Rejects a swap down to zero roots rather than panicking.
    let err = core.update_media_roots(vec![]).await.unwrap_err();
    assert!(matches!(err, swarm_server::ServerError::NoMediaRoots));

    std::fs::remove_dir_all(&base).ok();
}
