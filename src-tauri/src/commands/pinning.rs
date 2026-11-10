//! Tauri commands for pins and the upstream commit list (sm-69q.5).
//!
//! Thin wrappers over `core::pinning`; every call runs on a blocking thread.

use std::sync::Arc;

use tauri::State;

use crate::core::error::AppError;
use crate::core::pinning::{self, PinState, UpstreamCommits};
use crate::core::skill_store::SkillStore;

/// The skill's pin while it is pinned, else `null`.
#[tauri::command]
pub async fn get_skill_pin(skill_id: String) -> Result<Option<PinState>, AppError> {
    tauri::async_runtime::spawn_blocking(move || Ok(pinning::active(&skill_id))).await?
}

#[tauri::command]
pub async fn pin_skill(
    skill_id: String,
    note: Option<String>,
    store: State<'_, Arc<SkillStore>>,
) -> Result<PinState, AppError> {
    let store = store.inner().clone();
    tauri::async_runtime::spawn_blocking(move || pinning::pin(&store, &skill_id, note.as_deref()))
        .await?
}

#[tauri::command]
pub async fn unpin_skill(
    skill_id: String,
    store: State<'_, Arc<SkillStore>>,
) -> Result<PinState, AppError> {
    let store = store.inner().clone();
    tauri::async_runtime::spawn_blocking(move || pinning::unpin(&store, &skill_id)).await?
}

/// Upstream commits since the skill's baseline that touch its directory.
#[tauri::command]
pub async fn get_skill_upstream_commits(
    skill_id: String,
    store: State<'_, Arc<SkillStore>>,
) -> Result<UpstreamCommits, AppError> {
    let store = store.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        pinning::upstream_commits(&store, &skill_id, store.proxy_url().as_deref())
    })
    .await?
}
