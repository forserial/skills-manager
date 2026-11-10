//! Tauri commands for library folders with no skill record (sm-69q.16).
//!
//! Thin wrappers over `core::orphaned_folders`, on a blocking thread: listing
//! walks each folder for its size, and adopting hashes the folder.

use std::sync::Arc;

use tauri::State;

use crate::core::error::AppError;
use crate::core::orphaned_folders::{self, AdoptedFolder, OrphanedFolder};
use crate::core::skill_store::SkillStore;

/// Top-level library folders no skill record points at. Read-only.
#[tauri::command]
pub async fn get_orphaned_folders(
    store: State<'_, Arc<SkillStore>>,
) -> Result<Vec<OrphanedFolder>, AppError> {
    let store = store.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        orphaned_folders::find(&store).map_err(AppError::io)
    })
    .await?
}

/// Give one of them a record in place, as a new local skill.
#[tauri::command]
pub async fn adopt_orphaned_folder(
    dir_name: String,
    store: State<'_, Arc<SkillStore>>,
) -> Result<AdoptedFolder, AppError> {
    let store = store.inner().clone();
    tauri::async_runtime::spawn_blocking(move || orphaned_folders::adopt(&store, &dir_name))
        .await?
}
