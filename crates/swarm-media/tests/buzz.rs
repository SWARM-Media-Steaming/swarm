use std::path::Path;
use std::sync::Arc;
use swarm_core::peer::{BuzzRequest, BuzzResponse, LikeToggle, PeerRequest};
use swarm_media::scan::scan_root;
use swarm_media::serve::{Body, MediaService};
use swarm_media::store::Library;

fn request(value: &BuzzRequest) -> PeerRequest {
    let json = serde_json::to_vec(value).unwrap();
    PeerRequest {
        path: format!("/buzz?payload={}", hex::encode(json)),
        range: None,
        if_none_match: None,
        playback: None,
        error_report: None,
        like: None,
    }
}

fn decode(body: Body) -> BuzzResponse {
    let Body::Bytes(bytes) = body else {
        panic!("expected JSON")
    };
    serde_json::from_slice(&bytes).unwrap()
}

fn write(root: &Path, relative: &str) {
    let path = root.join(relative);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, [1u8; 16]).unwrap();
}

#[tokio::test]
async fn guided_session_persists_answers_and_rejection_changes_the_pick() {
    let base = std::env::temp_dir().join(format!(
        "swarm-buzz-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let root = base.join("media");
    write(&root, "Movies/Alpha (1995)/Alpha.mkv");
    write(&root, "Movies/Beta (2024)/Beta.mkv");
    let library = Arc::new(
        Library::open(base.join("library.sqlite").to_str().unwrap())
            .await
            .unwrap(),
    );
    scan_root(&library, &root).await.unwrap();
    let service = MediaService::new(library.clone(), root);

    let start = service
        .resolve_for_peer(
            &request(&BuzzRequest {
                action: "start".into(),
                session_id: None,
                profile_id: Some("adult".into()),
                value: Some("find_me_something".into()),
                media_id: None,
            }),
            true,
            "TV",
            "device-cert",
        )
        .await;
    let mut screen = decode(start.body);
    assert_eq!(screen.screen, "question");
    for value in ["funny", "movie", "dont_care"] {
        let response = service
            .resolve_for_peer(
                &request(&BuzzRequest {
                    action: "answer".into(),
                    session_id: Some(screen.session_id.clone()),
                    profile_id: None,
                    value: Some(value.into()),
                    media_id: None,
                }),
                true,
                "TV",
                "device-cert",
            )
            .await;
        screen = decode(response.body);
    }
    assert_eq!(screen.screen, "recommendation");
    let first = screen.media_id.clone().unwrap();
    let retry = service
        .resolve_for_peer(
            &request(&BuzzRequest {
                action: "not_interested".into(),
                session_id: Some(screen.session_id.clone()),
                profile_id: None,
                value: Some("not_for_me".into()),
                media_id: Some(first.clone()),
            }),
            true,
            "TV",
            "device-cert",
        )
        .await;
    let retry = decode(retry.body);
    assert_ne!(retry.media_id.as_deref(), Some(first.as_str()));

    let session = library
        .buzz_session(&screen.session_id, "device-cert")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(session.profile_id.as_deref(), Some("adult"));
    assert_eq!(session.answers.answered_questions, ["mood", "kind", "era"]);
    let history = library
        .buzz_history("device-cert", Some("adult"))
        .await
        .unwrap();
    assert_eq!(history.rejected.get(&first).unwrap().count, 1);

    // QUIC certificate identity, not a spoofable body id, is the shared key
    // between likes and Buzz history.
    let liked = service
        .resolve_for_peer(
            &PeerRequest {
                path: "/likes/toggle".into(),
                range: None,
                if_none_match: None,
                playback: None,
                error_report: None,
                like: Some(LikeToggle {
                    device_id: "spoofed".into(),
                    device_name: "TV".into(),
                    entry_key: first.clone(),
                    liked: true,
                }),
            },
            true,
            "TV",
            "device-cert",
        )
        .await;
    assert_eq!(liked.header.status, 204);
    assert!(library
        .buzz_history("device-cert", Some("adult"))
        .await
        .unwrap()
        .liked
        .contains(&first));
}

#[tokio::test]
async fn buzz_rejects_unauthenticated_and_cross_device_sessions() {
    let base = std::env::temp_dir().join(format!("swarm-buzz-auth-{}", std::process::id()));
    let root = base.join("media");
    std::fs::create_dir_all(&root).unwrap();
    let library = Arc::new(
        Library::open(base.join("library.sqlite").to_str().unwrap())
            .await
            .unwrap(),
    );
    let service = MediaService::new(library, root);
    let start = request(&BuzzRequest {
        action: "start".into(),
        session_id: None,
        profile_id: None,
        value: Some("surprise_me".into()),
        media_id: None,
    });
    assert_eq!(service.resolve(&start).await.header.status, 401);
}
