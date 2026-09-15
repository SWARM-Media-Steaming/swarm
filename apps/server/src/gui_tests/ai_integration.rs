//! Scenario category 7: the AI tab (issue #235) — provider settings
//! round-trip, the scan/scrape assist gate, and the reorganize
//! scan/approve/reject lifecycle. No test here ever calls a real AI
//! provider (that would need a live API key and network access this suite
//! deliberately never depends on — see `swarm-media-server-uat-tests`); the
//! reorganize scans below use only filenames `classify` already parses
//! confidently, which needs no AI client at all (`ai: None` in
//! `reorganize::scan_root`), and the provider/gating tests only exercise
//! the settings round-trip and the "not configured yet" error paths.
//!
//! Issue #296 removed the separate `ai_scan_assist_enabled`/
//! `ai_reorganize_enabled` toggles: both features are always available now,
//! gated only by an enabled+ready AI provider (indirect permission, granted
//! once on the "Enabled AI tools" panel) and the explicit action itself
//! (clicking "Ask AI"/"Check now"/"Scan for cleanup" — direct permission).

use super::harness::{empty_media_root_dir, test_app, test_app_with_media_root};
use crate::{
    add_media_root, ai_reorganize_scan, ai_scrape_assist, approve_ai_reorg_plan, get_settings,
    list_ai_reorg_plans, list_scrape_issues, reject_ai_reorg_plan, run_scrape_assist_now,
    set_ai_provider_api_key, set_ai_provider_enabled, set_ai_provider_model, test_ai_provider,
};
use tauri::Manager;

#[tokio::test]
async fn ai_providers_default_to_the_three_known_providers_disabled_and_keyless() {
    let test_app = test_app();
    let app = test_app.handle();

    let settings = get_settings(app.clone()).await.expect("get_settings should succeed");
    let ids: Vec<&str> = settings.ai_providers.iter().map(|p| p.id.as_str()).collect();
    assert_eq!(ids, ["claude", "codex", "grok"]);
    assert!(settings.ai_providers.iter().all(|p| !p.enabled && !p.has_api_key));
}

#[tokio::test]
async fn ai_provider_settings_round_trip_without_ever_returning_the_raw_key() {
    let test_app = test_app();
    let app = test_app.handle();

    set_ai_provider_enabled(app.clone(), "claude".to_string(), true)
        .await
        .expect("set_ai_provider_enabled should succeed");
    set_ai_provider_model(app.clone(), "claude".to_string(), "claude-opus-5".to_string())
        .await
        .expect("set_ai_provider_model should succeed");
    set_ai_provider_api_key(app.clone(), "claude".to_string(), "sk-test-secret".to_string())
        .await
        .expect("set_ai_provider_api_key should succeed");

    let settings = get_settings(app.clone()).await.expect("get_settings should succeed");
    let claude = settings.ai_providers.iter().find(|p| p.id == "claude").unwrap();
    assert!(claude.enabled);
    assert_eq!(claude.model, "claude-opus-5");
    assert!(claude.has_api_key, "a saved key should be reported as present");

    // The DTO sent to the frontend never carries the raw key — only
    // `has_api_key` — so nothing in `SettingsView` can leak it back out.
    let serialized = serde_json::to_string(&settings).expect("SettingsView should serialize");
    assert!(!serialized.contains("sk-test-secret"));
}

#[tokio::test]
async fn set_ai_provider_enabled_rejects_an_unknown_provider_id() {
    let test_app = test_app();
    let app = test_app.handle();

    let error = set_ai_provider_enabled(app.clone(), "not-a-real-provider".to_string(), true)
        .await
        .expect_err("an unknown provider id should be rejected");
    assert!(error.contains("not-a-real-provider"));
}

#[tokio::test]
async fn test_ai_provider_reports_the_cli_detection_state() {
    // Issue #252: no API key involved — `test_ai_provider` detects the
    // provider's CLI. On a machine without the Claude CLI installed and
    // signed in (the CI default) this reports "not installed" / "not signed
    // in"; where it *is* present it returns a "detected" string. Either way
    // the result is deterministic for a given machine and never panics.
    match test_ai_provider("claude".to_string()).await {
        Ok(message) => assert!(message.contains("detected")),
        Err(message) => assert!(
            message.contains("not installed") || message.contains("not signed in"),
            "unexpected detection error: {message}"
        ),
    }
}

#[tokio::test]
async fn test_ai_provider_rejects_an_unknown_provider_id() {
    let error = test_ai_provider("not-a-real-provider".to_string())
        .await
        .expect_err("an unknown provider id should be rejected");
    assert!(error.contains("not-a-real-provider"));
}

#[tokio::test]
async fn list_scrape_issues_is_empty_before_any_scrape_has_run() {
    let test_app = test_app();
    let app = test_app.handle();

    let issues = list_scrape_issues(app.state()).await.expect("list_scrape_issues should succeed");
    assert!(issues.is_empty());
}

#[tokio::test]
async fn ai_scrape_assist_refuses_to_run_without_an_enabled_ai_provider() {
    let test_app = test_app();
    let app = test_app.handle();

    let error = ai_scrape_assist(app.clone(), app.state(), "whatever-entry-key".to_string())
        .await
        .expect_err("scan assist should refuse to run with no provider enabled");
    assert!(error.contains("Enable"));
}

#[tokio::test]
async fn run_scrape_assist_now_refuses_to_run_without_an_enabled_ai_provider() {
    // Same gate as the per-item command — clicking "Check now" with no AI
    // provider enabled must not silently no-op, it should report exactly
    // why, same as ai_scrape_assist does.
    let test_app = test_app();
    let app = test_app.handle();

    let error = run_scrape_assist_now(app.clone(), app.state())
        .await
        .expect_err("scan assist should refuse to run with no provider enabled");
    assert!(error.contains("Enable"));
}

#[tokio::test]
async fn ai_reorganize_scan_rejects_an_unknown_media_root() {
    // Reorganize has no enable gate any more (issue #296) — scanning
    // succeeds without any AI provider configured (see the test below); the
    // only way this command fails on a fresh app is an unresolvable root.
    let test_app = test_app();
    let app = test_app.handle();

    let error = ai_reorganize_scan(app.clone(), app.state(), "Movies".to_string())
        .await
        .expect_err("an unconfigured media root should be rejected");
    assert!(error.contains("unknown media root"));
}

#[tokio::test]
async fn ai_reorganize_scan_proposes_a_plan_for_a_messy_filename_with_no_ai_needed() {
    let (test_app, root_dir) = test_app_with_media_root().await;
    let app = test_app.handle();

    std::fs::write(
        root_dir.path().join("10.Cloverfield.Lane.2016.1080p.BluRay.x264-GROUP.mkv"),
        b"fake video bytes",
    )
    .expect("write fixture movie file");

    let plan = ai_reorganize_scan(app.clone(), app.state(), "Movies".to_string())
        .await
        .expect("ai_reorganize_scan should succeed against a real, isolated media root");
    assert_eq!(plan.status, "proposed");
    assert_eq!(plan.items.len(), 1);
    assert_eq!(
        plan.items[0].to,
        "10 Cloverfield Lane (2016)/10 Cloverfield Lane (2016).mkv"
    );
    assert!(plan.items[0].conflict.is_none());
    assert_eq!(plan.ai_assisted_count, 0, "classify() already understood this name, no AI needed");

    let plans = list_ai_reorg_plans(app.state()).await.expect("list_ai_reorg_plans should succeed");
    assert_eq!(plans.len(), 1);
    assert_eq!(plans[0].id, plan.id);
}

/// Issue #301: a TV show bundle sitting in a `Movies`-typed root, with a
/// second `Shows`-typed root also configured, is reported (not moved) as
/// belonging in the other root — and never shows up as a normal move
/// proposal for the wrong reason.
#[tokio::test]
async fn ai_reorganize_scan_reports_an_episode_shaped_file_sitting_in_the_movies_root() {
    let (test_app, movies_dir) = test_app_with_media_root().await;
    let app = test_app.handle();
    let shows_dir = empty_media_root_dir();
    add_media_root(
        app.clone(),
        app.state(),
        "Shows".to_string(),
        shows_dir.path().to_string_lossy().to_string(),
        Some("shows".to_string()),
    )
    .await
    .expect("add_media_root should succeed for the second, Shows-typed root");

    std::fs::write(
        movies_dir.path().join("Dragon.Ball.Super.S01E01.mkv"),
        b"fake video bytes",
    )
    .expect("write fixture episode file into the Movies root");

    let plan = ai_reorganize_scan(app.clone(), app.state(), "Movies".to_string())
        .await
        .expect("ai_reorganize_scan should succeed");

    assert_eq!(plan.misplaced.len(), 1);
    assert_eq!(plan.misplaced[0].path, "Dragon.Ball.Super.S01E01.mkv");
    assert_eq!(plan.misplaced[0].kind, "episode");
    assert_eq!(plan.misplaced[0].current_root_label, "Movies");
    assert_eq!(plan.misplaced[0].correct_root_label, "Shows");
}

#[tokio::test]
async fn approve_ai_reorg_plan_moves_the_file_and_never_deletes_anything() {
    let (test_app, root_dir) = test_app_with_media_root().await;
    let app = test_app.handle();
    std::fs::write(root_dir.path().join("Heat.1995.mkv"), b"fake video bytes").expect("write fixture movie file");

    let plan = ai_reorganize_scan(app.clone(), app.state(), "Movies".to_string())
        .await
        .expect("ai_reorganize_scan should succeed");

    let applying = approve_ai_reorg_plan(app.clone(), app.state(), plan.id, Vec::new())
        .await
        .expect("approve_ai_reorg_plan should succeed");
    assert_eq!(applying.status, "applying");
    assert!(applying.apply_summary.is_none());

    let applied = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let plans = list_ai_reorg_plans(app.state())
                .await
                .expect("list_ai_reorg_plans should succeed");
            if plans[0].status == "applied" {
                break plans.into_iter().next().expect("stored plan");
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("background reorganization should finish");
    let summary = applied.apply_summary.expect("an applied plan should carry a summary");
    assert_eq!(summary.applied, 1);
    assert_eq!(summary.skipped, 0);

    assert!(
        !root_dir.path().join("Heat.1995.mkv").exists(),
        "the original path should be gone — moved, not copied"
    );
    assert!(root_dir
        .path()
        .join("Heat (1995)/Heat (1995).mkv")
        .exists());

    // Approving again must be rejected — this is a one-shot action, not an
    // idempotent replay (the source file it would move no longer exists at
    // its original path anyway).
    let error = approve_ai_reorg_plan(app.clone(), app.state(), plan.id, Vec::new())
        .await
        .expect_err("re-approving an already-applied plan should fail");
    assert!(error.contains("already"));
}

#[tokio::test]
async fn approve_ai_reorg_plan_leaves_excluded_items_untouched() {
    // Issue #312: the review UI lets a user uncheck individual items before
    // approving. That must leave the excluded file exactly where it was —
    // neither moved nor counted as "skipped" (which is reserved for items
    // the plan itself couldn't apply, e.g. a conflict) — while every other
    // item in the same plan still goes through.
    let (test_app, root_dir) = test_app_with_media_root().await;
    let app = test_app.handle();
    std::fs::write(root_dir.path().join("Heat.1995.mkv"), b"fake video bytes").expect("write fixture movie file");
    std::fs::write(root_dir.path().join("Se7en.1995.mkv"), b"fake video bytes").expect("write fixture movie file");

    let plan = ai_reorganize_scan(app.clone(), app.state(), "Movies".to_string())
        .await
        .expect("ai_reorganize_scan should succeed");
    assert_eq!(plan.items.len(), 2);

    let excluded_item = plan
        .items
        .iter()
        .find(|item| item.from == "Heat.1995.mkv")
        .expect("Heat.1995.mkv should be in the plan");

    approve_ai_reorg_plan(app.clone(), app.state(), plan.id, vec![excluded_item.from.clone()])
        .await
        .expect("approve_ai_reorg_plan should succeed");

    let applied = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let plans = list_ai_reorg_plans(app.state())
                .await
                .expect("list_ai_reorg_plans should succeed");
            if plans[0].status == "applied" {
                break plans.into_iter().next().expect("stored plan");
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("background reorganization should finish");
    let summary = applied.apply_summary.expect("an applied plan should carry a summary");
    assert_eq!(summary.applied, 1, "only the non-excluded item should be applied");
    assert_eq!(summary.skipped, 0, "an excluded item is not a skip — it was never attempted");

    assert!(
        root_dir.path().join("Heat.1995.mkv").exists(),
        "the excluded item must be left exactly where it was"
    );
    assert!(
        !root_dir.path().join("Se7en.1995.mkv").exists(),
        "the non-excluded item should have moved"
    );
    assert!(root_dir.path().join("Se7en (1995)/Se7en (1995).mkv").exists());
}

#[tokio::test]
async fn reject_ai_reorg_plan_leaves_the_filesystem_untouched() {
    let (test_app, root_dir) = test_app_with_media_root().await;
    let app = test_app.handle();
    std::fs::write(root_dir.path().join("Heat.1995.mkv"), b"fake video bytes").expect("write fixture movie file");

    let plan = ai_reorganize_scan(app.clone(), app.state(), "Movies".to_string())
        .await
        .expect("ai_reorganize_scan should succeed");

    reject_ai_reorg_plan(app.state(), plan.id)
        .await
        .expect("reject_ai_reorg_plan should succeed");

    assert!(
        root_dir.path().join("Heat.1995.mkv").exists(),
        "rejecting a plan must never touch the filesystem"
    );
    let plans = list_ai_reorg_plans(app.state()).await.expect("list_ai_reorg_plans should succeed");
    assert_eq!(plans[0].status, "rejected");
}
