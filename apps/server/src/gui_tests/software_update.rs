//! Scenario category: issue #309's "Automatically" update mode waits for
//! `active_playback_sessions` to hit zero before installing. This covers the
//! helper that reads that count — reusing the exact figure the Metrics tab
//! already shows rather than a separate "busy" concept — for both the
//! not-yet-configured and real-but-idle `ServerCore` cases. The GitHub
//! Releases API calls behind `check_for_update`/`install_update` are
//! deliberately not exercised here: they need a live network call against
//! real, signed releases this suite has no way to produce, so that coverage
//! lives in `gui.rs`'s pure `update_candidates`/`release_is_newer` unit
//! tests instead.

use super::harness::{test_app, test_app_with_media_root};
use crate::active_playback_sessions;

#[tokio::test]
async fn zero_when_no_core_has_started() {
    let test_app = test_app();
    let app = test_app.handle();
    assert_eq!(active_playback_sessions(&app).await, 0);
}

#[tokio::test]
async fn zero_for_a_real_idle_core() {
    let (test_app, _root_dir) = test_app_with_media_root().await;
    let app = test_app.handle();
    assert_eq!(active_playback_sessions(&app).await, 0);
}
