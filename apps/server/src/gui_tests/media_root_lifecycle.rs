//! Scenario category 1: media root lifecycle (add / list / remove), backed
//! only by the settings file — no `ServerCore` is started by these
//! commands unless one is already running, so these tests never bind a
//! network port.

use super::harness::{empty_media_root_dir, test_app};
use crate::{add_media_root, list_media_roots, remove_media_root};
use tauri::Manager;

#[tokio::test]
async fn add_media_root_persists_and_is_listed() {
    let test_app = test_app();
    let app = test_app.handle();
    let root_dir = empty_media_root_dir();

    let result = add_media_root(
        app.clone(),
        app.state(),
        "Movies".to_string(),
        root_dir.path().to_string_lossy().to_string(),
        Some("movies".to_string()),
    )
    .await
    .expect("add_media_root should succeed for a real, empty, on-disk directory");

    assert_eq!(result.media_roots.len(), 1);
    assert_eq!(result.media_roots[0].label, "Movies");
    assert_eq!(
        result.media_roots[0].asset_type,
        crate::settings::RootAssetType::Movies
    );

    let listed = list_media_roots(app.clone())
        .await
        .expect("list_media_roots should succeed");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].label, "Movies");
}

#[tokio::test]
async fn add_media_root_derives_a_label_when_none_is_given() {
    let test_app = test_app();
    let app = test_app.handle();
    let root_dir = empty_media_root_dir();
    let named = root_dir.path().join("Family Movies");
    std::fs::create_dir(&named).expect("create named media folder");

    let result = add_media_root(
        app.clone(),
        app.state(),
        String::new(),
        named.to_string_lossy().to_string(),
        None,
    )
    .await
    .expect("add_media_root should derive a label from the folder name");

    // "Family Movies" -> filesystem-safe "Family-Movies".
    assert_eq!(result.media_roots[0].label, "Family-Movies");
    assert_eq!(
        result.media_roots[0].asset_type,
        crate::settings::RootAssetType::Mixed
    );
}

#[tokio::test]
async fn add_media_root_rejects_duplicate_label() {
    let test_app = test_app();
    let app = test_app.handle();
    let first_dir = empty_media_root_dir();
    let second_dir = empty_media_root_dir();

    add_media_root(
        app.clone(),
        app.state(),
        "Movies".to_string(),
        first_dir.path().to_string_lossy().to_string(),
        None,
    )
    .await
    .expect("first add should succeed");

    let err = add_media_root(
        app.clone(),
        app.state(),
        "Movies".to_string(),
        second_dir.path().to_string_lossy().to_string(),
        None,
    )
    .await
    .expect_err("a second root with the same label must be rejected");
    assert!(
        err.contains("already exists"),
        "expected a duplicate-label error, got: {err}"
    );
}

#[tokio::test]
async fn add_media_root_rejects_the_same_folder_twice() {
    let test_app = test_app();
    let app = test_app.handle();
    let root_dir = empty_media_root_dir();

    add_media_root(
        app.clone(),
        app.state(),
        "Movies".to_string(),
        root_dir.path().to_string_lossy().to_string(),
        None,
    )
    .await
    .expect("first add should succeed");

    let err = add_media_root(
        app.clone(),
        app.state(),
        "Shows".to_string(),
        root_dir.path().to_string_lossy().to_string(),
        None,
    )
    .await
    .expect_err("issue #252: the same folder must not be added as a second root");
    assert!(
        err.contains("already added"),
        "expected a duplicate-folder error, got: {err}"
    );

    let listed = list_media_roots(app.clone())
        .await
        .expect("list_media_roots should succeed");
    assert_eq!(listed.len(), 1, "the rejected add must not have mutated settings");
}

#[tokio::test]
async fn remove_media_root_allows_removing_the_last_root() {
    let test_app = test_app();
    let app = test_app.handle();
    let root_dir = empty_media_root_dir();

    add_media_root(
        app.clone(),
        app.state(),
        "Movies".to_string(),
        root_dir.path().to_string_lossy().to_string(),
        None,
    )
    .await
    .expect("add should succeed");

    let result = remove_media_root(app.clone(), app.state(), "Movies".to_string())
        .await
        .expect("issue #252: removing the only remaining root is allowed");
    assert!(result.media_roots.is_empty());
    assert!(result.rescan.is_none());

    let listed = list_media_roots(app.clone())
        .await
        .expect("list_media_roots should succeed");
    assert!(
        listed.is_empty(),
        "removing the last root leaves settings with none configured"
    );
}

#[tokio::test]
async fn remove_media_root_deletes_a_non_last_root() {
    let test_app = test_app();
    let app = test_app.handle();
    let first_dir = empty_media_root_dir();
    let second_dir = empty_media_root_dir();

    add_media_root(
        app.clone(),
        app.state(),
        "Movies".to_string(),
        first_dir.path().to_string_lossy().to_string(),
        None,
    )
    .await
    .expect("first add should succeed");
    add_media_root(
        app.clone(),
        app.state(),
        "Shows".to_string(),
        second_dir.path().to_string_lossy().to_string(),
        None,
    )
    .await
    .expect("second add should succeed");

    let result = remove_media_root(app.clone(), app.state(), "Movies".to_string())
        .await
        .expect("removing one of two roots should succeed");
    assert_eq!(result.media_roots.len(), 1);
    assert_eq!(result.media_roots[0].label, "Shows");
}
