//! Scenario category: transcoding controls (encoder mode / max resolution /
//! segment length). Settings-only — the commands persist to `settings.json`
//! and, when a `ServerCore` is running, push the value into the live
//! `TranscodeManager`; `test_app()` has no core, so this covers the persist +
//! round-trip half. The live-apply half is covered by `transcode.rs`'s own
//! `TranscodeManager` tests.

use super::harness::test_app;
use crate::{
    get_settings, set_auto_update, set_hls_segment_seconds, set_max_transcode_height,
    set_video_encoder_mode,
};
use tauri::Manager;

#[tokio::test]
async fn auto_update_mode_persists_and_rejects_unknown_values() {
    let test_app = test_app();
    let app = test_app.handle();

    assert_eq!(
        get_settings(app.clone()).await.unwrap().auto_update,
        "notify",
        "default is notify"
    );

    set_auto_update(app.clone(), "auto".to_string())
        .await
        .expect("set auto");
    assert_eq!(get_settings(app.clone()).await.unwrap().auto_update, "auto");

    assert!(set_auto_update(app.clone(), "sometimes".to_string())
        .await
        .is_err());
    assert_eq!(
        get_settings(app.clone()).await.unwrap().auto_update,
        "auto",
        "a rejected value leaves the previous one intact"
    );

    // Issue #309: "off" was removed from the UI — an existing settings.json
    // still holding it upgrades to "notify" on load (settings::tests), and
    // this command no longer accepts setting it going forward either.
    assert!(set_auto_update(app.clone(), "off".to_string())
        .await
        .is_err());
}

#[tokio::test]
async fn video_encoder_mode_persists_and_normalizes() {
    let test_app = test_app();
    let app = test_app.handle();

    let initial = get_settings(app.clone()).await.expect("get_settings");
    assert_eq!(initial.video_encoder_mode, "auto", "default is auto");

    set_video_encoder_mode(app.clone(), app.state(), "hardware".to_string())
        .await
        .expect("set hardware");
    assert_eq!(
        get_settings(app.clone()).await.unwrap().video_encoder_mode,
        "hardware"
    );

    // Anything unrecognized normalizes back to a known value rather than
    // persisting garbage.
    set_video_encoder_mode(app.clone(), app.state(), "nonsense".to_string())
        .await
        .expect("set nonsense normalizes");
    assert_eq!(
        get_settings(app.clone()).await.unwrap().video_encoder_mode,
        "auto"
    );

    set_video_encoder_mode(app.clone(), app.state(), "software".to_string())
        .await
        .expect("set software");
    assert_eq!(
        get_settings(app.clone()).await.unwrap().video_encoder_mode,
        "software"
    );
}

#[tokio::test]
async fn max_transcode_height_round_trips_including_the_no_cap_sentinel() {
    let test_app = test_app();
    let app = test_app.handle();

    assert_eq!(
        get_settings(app.clone()).await.unwrap().max_transcode_height,
        0
    );

    set_max_transcode_height(app.clone(), app.state(), 1080)
        .await
        .expect("cap at 1080");
    assert_eq!(
        get_settings(app.clone()).await.unwrap().max_transcode_height,
        1080
    );

    set_max_transcode_height(app.clone(), app.state(), 0)
        .await
        .expect("back to no cap");
    assert_eq!(
        get_settings(app.clone()).await.unwrap().max_transcode_height,
        0
    );
}

#[tokio::test]
async fn hls_segment_seconds_persists_and_is_floored_at_two() {
    let test_app = test_app();
    let app = test_app.handle();

    assert_eq!(
        get_settings(app.clone()).await.unwrap().hls_segment_seconds,
        4
    );

    set_hls_segment_seconds(app.clone(), app.state(), 2)
        .await
        .expect("2s");
    assert_eq!(
        get_settings(app.clone()).await.unwrap().hls_segment_seconds,
        2
    );

    // Below the floor is clamped, not rejected.
    set_hls_segment_seconds(app.clone(), app.state(), 0)
        .await
        .expect("0 clamps");
    assert_eq!(
        get_settings(app.clone()).await.unwrap().hls_segment_seconds,
        2
    );
}
