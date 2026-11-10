//! Tauri commands for watched upstreams and fork (sm-69q.7).
//!
//! Thin wrappers over `core::watched_upstreams`; every call runs on a blocking
//! thread because checks and "mark as seen" resolve remotes over the network.

use std::sync::Arc;

use tauri::State;

use crate::core::error::AppError;
use crate::core::skill_store::SkillStore;
use crate::core::watched_upstreams::{self, ForkResult, WatchedUpstream};

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
