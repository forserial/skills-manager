//! Tauri commands for a merge held over conflicts (sm-69q.6).
//!
//! Thin wrappers over `core::skill_update_merge`; every call runs on a
//! blocking thread. Applying is `update_skill` again: once every conflict
//! has a choice, the update merges with those choices and applies.

use std::path::PathBuf;
use std::sync::Arc;

use tauri::State;

use crate::core::error::AppError;
use crate::core::file_manager;
use crate::core::skill_store::{SkillRecord, SkillStore};
use crate::core::skill_update_merge::{self as merge, AbortReport, MergeStatus, Take};

fn skill_by_id(store: &SkillStore, skill_id: &str) -> Result<SkillRecord, AppError> {
    store
        .get_skill_by_id(skill_id)
        .map_err(AppError::db)?
        .ok_or_else(|| AppError::not_found("Skill not found"))
}

/// The skill's pending merge (`pending: false` when there is none).
#[tauri::command]
pub async fn get_skill_merge(
    skill_id: String,
    store: State<'_, Arc<SkillStore>>,
) -> Result<MergeStatus, AppError> {
    let store = store.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        merge::status(&skill_by_id(&store, &skill_id)?).map_err(AppError::internal)
    })
    .await?
}

/// Keep mine (`local`) or take theirs (`upstream`) for one conflicted file.
/// Supplying edited content is the CLI's `--file`.
#[tauri::command]
pub async fn resolve_skill_merge(
    skill_id: String,
    path: String,
    take: Take,
    store: State<'_, Arc<SkillStore>>,
) -> Result<MergeStatus, AppError> {
    if take == Take::File {
        return Err(AppError::invalid_input(
            "supplying a file is only available from the CLI (`skills merge resolve --file`)",
        ));
    }
    let store = store.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let skill = skill_by_id(&store, &skill_id)?;
        merge::resolve(&skill, &path, take, None)
            .map_err(|e| AppError::invalid_input(format!("{e:#}")))
    })
    .await?
}

/// Drop the pending merge. The library was never touched by it.
#[tauri::command]
pub async fn abort_skill_merge(
    skill_id: String,
    store: State<'_, Arc<SkillStore>>,
) -> Result<AbortReport, AppError> {
    let store = store.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        merge::abort(&skill_by_id(&store, &skill_id)?).map_err(AppError::internal)
    })
    .await?
}

/// Open the folder holding the versions of one conflicted file ("open both
/// versions"). The folder is resolved from the pending record, never taken
/// from the frontend, so no other path can be opened this way.
#[tauri::command]
pub async fn reveal_skill_merge_versions(
    skill_id: String,
    path: String,
    store: State<'_, Arc<SkillStore>>,
) -> Result<(), AppError> {
    let store = store.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let skill = skill_by_id(&store, &skill_id)?;
        let status = merge::status(&skill).map_err(AppError::internal)?;
        let dir = status
            .conflicts
            .iter()
            .find(|c| c.path == path)
            .map(|c| PathBuf::from(&c.versions_dir))
            .ok_or_else(|| AppError::not_found("No pending merge conflict for that file"))?;
        if !dir.is_dir() {
            return Err(AppError::not_found(format!(
                "The versions folder is missing: {}",
                dir.display()
            )));
        }
        let status = file_manager::open_dir(&dir)
            .map_err(|e| AppError::io(format!("Failed to open folder: {e}")))?;
        if file_manager::exit_is_failure(status.success()) {
            return Err(AppError::io(format!(
                "File manager exited with status: {status}"
            )));
        }
        Ok(())
    })
    .await?
}
