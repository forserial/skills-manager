//! Tauri commands for watched upstreams and fork (sm-69q.7).
//!
//! Thin wrappers over `core::watched_upstreams`; every call runs on a blocking
//! thread because checks and "mark as seen" resolve remotes over the network.

use std::sync::Arc;

use tauri::State;

use crate::core::error::AppError;
use crate::core::skill_store::SkillStore;
use crate::core::watched_upstreams::batch::{self, WatchedMarker};
use crate::core::watched_upstreams::{self, ForkResult, NewWatch, WatchedUpstream};

/// A skill's watched upstreams as stored, without touching the network.
#[tauri::command]
pub async fn get_watched_upstreams(skill_id: String) -> Result<Vec<WatchedUpstream>, AppError> {
    tauri::async_runtime::spawn_blocking(move || watched_upstreams::list(&skill_id)).await?
}

/// Resolve each watched upstream's remote revision and store what moved.
#[tauri::command]
pub async fn check_watched_upstreams(
    skill_id: String,
    store: State<'_, Arc<SkillStore>>,
) -> Result<Vec<WatchedUpstream>, AppError> {
    let store = store.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        watched_upstreams::check(&skill_id, store.proxy_url().as_deref())
    })
    .await?
}

/// "I have looked": the current remote revision becomes the last seen one.
#[tauri::command]
pub async fn mark_watched_upstream_seen(
    skill_id: String,
    watch_id: String,
    store: State<'_, Arc<SkillStore>>,
) -> Result<WatchedUpstream, AppError> {
    let store = store.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        watched_upstreams::mark_seen(&store, &skill_id, &watch_id, store.proxy_url().as_deref())
    })
    .await?
}

/// Watch a git location (sm-69q.13). Without a revision, the remote's current
/// head is what counts as seen.
#[tauri::command]
pub async fn add_watched_upstream(
    skill_id: String,
    git_url: String,
    subpath: Option<String>,
    branch: Option<String>,
    note: Option<String>,
    store: State<'_, Arc<SkillStore>>,
) -> Result<WatchedUpstream, AppError> {
    let store = store.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let input = NewWatch {
            git_url,
            subpath,
            branch,
            revision: None,
            note,
        };
        watched_upstreams::add(&store, &skill_id, input, store.proxy_url().as_deref())
            .map_err(without_cli_hint)
    })
    .await?
}

/// Stop watching a location (sm-69q.13).
#[tauri::command]
pub async fn remove_watched_upstream(
    skill_id: String,
    watch_id: String,
    store: State<'_, Arc<SkillStore>>,
) -> Result<WatchedUpstream, AppError> {
    let store = store.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        watched_upstreams::remove(&store, &skill_id, &watch_id)
    })
    .await?
}

/// Skill-card markers: every skill that watches something, with how many of
/// its locations moved. No network.
#[tauri::command]
pub async fn get_watched_upstream_markers(
    store: State<'_, Arc<SkillStore>>,
) -> Result<Vec<WatchedMarker>, AppError> {
    let store = store.inner().clone();
    Ok(tauri::async_runtime::spawn_blocking(move || batch::markers(&store)).await?)
}

/// `add`'s lookup error ends with advice for the CLI (`--revision`), which the
/// app has no field for.
fn without_cli_hint(mut err: AppError) -> AppError {
    if let Some(at) = err.message.find(" Pass --revision") {
        err.message.truncate(at);
    }
    err
}

/// Copy a library skill to a new local skill that watches the original.
#[tauri::command]
pub async fn fork_skill(
    skill_id: String,
    new_name: String,
    store: State<'_, Arc<SkillStore>>,
) -> Result<ForkResult, AppError> {
    let store = store.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        watched_upstreams::fork_skill(&store, &skill_id, &new_name)
    })
    .await?
}
