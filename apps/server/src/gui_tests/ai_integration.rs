//! Scenario category 7: the AI tab (issue #235) — provider settings
//! round-trip, the scan/scrape assist gate, and the reorganize
//! scan/approve/reject lifecycle. No test here ever calls a real AI
//! provider (that would need a live API key and network access this suite
//! deliberately never depends on — see `swarm-media-server-uat-tests`); the
//! reorganize scans below use only filenames `classify` already parses
//! confidently, which needs no AI client at all (`ai: None` in
//! `reorganize::scan_root`), and the provider/gating tests only exercise
//! the settings round-trip and the "not configured yet" error paths.

use super::harness::{test_app, test_app_with_media_root};
use crate::{
    ai_reorganize_scan, ai_scrape_assist, approve_ai_reorg_plan, get_settings, list_ai_reorg_plans,
    list_scrape_issues, reject_ai_reorg_plan, set_ai_provider_api_key, set_ai_provider_enabled,
    set_ai_provider_model, set_ai_reorganize_enabled, set_ai_scan_assist_enabled, test_ai_provider,
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
    assert!(!settings.ai_scan_assist_enabled);
    assert!(!settings.ai_reorganize_enabled);
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
async fn ai_scan_assist_and_reorganize_toggles_persist_and_default_off() {
    let test_app = test_app();
    let app = test_app.handle();

    set_ai_scan_assist_enabled(app.clone(), true)
        .await
        .expect("set_ai_scan_assist_enabled should succeed");
    set_ai_reorganize_enabled(app.clone(), true)
        .await
        .expect("set_ai_reorganize_enabled should succeed");

    let settings = get_settings(app.clone()).await.expect("get_settings should succeed");
    assert!(settings.ai_scan_assist_enabled);
    assert!(settings.ai_reorganize_enabled);
}

#[tokio::test]
async fn list_scrape_issues_is_empty_before_any_scrape_has_run() {
    let test_app = test_app();
    let app = test_app.handle();

    let issues = list_scrape_issues(app.state()).await.expect("list_scrape_issues should succeed");
    assert!(issues.is_empty());
}

#[tokio::test]
async fn ai_scrape_assist_refuses_to_run_until_scan_assist_is_enabled() {
    let test_app = test_app();
    let app = test_app.handle();

    let error = ai_scrape_assist(app.clone(), app.state(), "whatever-entry-key".to_string())
        .await
        .expect_err("scan assist should refuse to run while disabled");
    assert!(error.contains("Enable"));
}

#[tokio::test]
async fn ai_reorganize_scan_refuses_to_run_until_reorganize_is_enabled() {
    let test_app = test_app();
    let app = test_app.handle();

    let error = ai_reorganize_scan(app.clone(), app.state(), "Movies".to_string())
        .await
        .expect_err("reorganize should refuse to run while disabled");
    assert!(error.contains("Enable"));
}

#[tokio::test]
async fn ai_reorganize_scan_proposes_a_plan_for_a_messy_filename_with_no_ai_needed() {
    let (test_app, root_dir) = test_app_with_media_root().await;
    let app = test_app.handle();
    set_ai_reorganize_enabled(app.clone(), true)
        .await
        .expect("set_ai_reorganize_enabled should succeed");

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
        "Movies/10 Cloverfield Lane (2016)/10 Cloverfield Lane (2016).mkv"
    );
    assert!(plan.items[0].conflict.is_none());
    assert_eq!(plan.ai_assisted_count, 0, "classify() already understood this name, no AI needed");

    let plans = list_ai_reorg_plans(app.state()).await.expect("list_ai_reorg_plans should succeed");
    assert_eq!(plans.len(), 1);
    assert_eq!(plans[0].id, plan.id);
}

#[tokio::test]
async fn approve_ai_reorg_plan_moves_the_file_and_never_deletes_anything() {
    let (test_app, root_dir) = test_app_with_media_root().await;
    let app = test_app.handle();
    set_ai_reorganize_enabled(app.clone(), true)
        .await
        .expect("set_ai_reorganize_enabled should succeed");
    std::fs::write(root_dir.path().join("Heat.1995.mkv"), b"fake video bytes").expect("write fixture movie file");

    let plan = ai_reorganize_scan(app.clone(), app.state(), "Movies".to_string())
        .await
        .expect("ai_reorganize_scan should succeed");

    let applied = approve_ai_reorg_plan(app.clone(), app.state(), plan.id)
        .await
        .expect("approve_ai_reorg_plan should succeed");
    assert_eq!(applied.status, "applied");
    let summary = applied.apply_summary.expect("an applied plan should carry a summary");
    assert_eq!(summary.applied, 1);
    assert_eq!(summary.skipped, 0);

    assert!(
        !root_dir.path().join("Heat.1995.mkv").exists(),
        "the original path should be gone — moved, not copied"
    );
    assert!(root_dir
        .path()
        .join("Movies/Heat (1995)/Heat (1995).mkv")
        .exists());

    // Approving again must be rejected — this is a one-shot action, not an
    // idempotent replay (the source file it would move no longer exists at
    // its original path anyway).
    let error = approve_ai_reorg_plan(app.clone(), app.state(), plan.id)
        .await
        .expect_err("re-approving an already-applied plan should fail");
    assert!(error.contains("already"));
}

#[tokio::test]
async fn reject_ai_reorg_plan_leaves_the_filesystem_untouched() {
    let (test_app, root_dir) = test_app_with_media_root().await;
    let app = test_app.handle();
    set_ai_reorganize_enabled(app.clone(), true)
        .await
        .expect("set_ai_reorganize_enabled should succeed");
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
