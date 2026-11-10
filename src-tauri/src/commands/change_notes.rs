//! Tauri commands for change notes and patch export (sm-69q.8).
//!
//! Thin wrappers over `core::change_notes`, each on a blocking thread: they
//! hash the skill's files, and the export may fetch the baseline revision.

use std::path::PathBuf;
use std::sync::Arc;

use tauri::State;

use crate::core::change_notes::{self, ChangesReport, PatchExport};
use crate::core::error::AppError;
use crate::core::skill_store::{SkillRecord, SkillStore};

fn find_skill(store: &SkillStore, skill_id: &str) -> Result<SkillRecord, AppError> {
    store
        .get_skill_by_id(skill_id)
        .map_err(AppError::db)?
        .ok_or_else(|| AppError::not_found("Skill not found"))
}

fn internal(err: anyhow::Error) -> AppError {
    AppError::internal(format!("{err:#}"))
}

/// The local changes of a skill with each file's note.
#[tauri::command]
pub async fn get_skill_change_notes(
    skill_id: String,
    store: State<'_, Arc<SkillStore>>,
) -> Result<ChangesReport, AppError> {
    let store = store.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let skill = find_skill(&store, &skill_id)?;
        change_notes::changes_report(&store, &skill).map_err(internal)
    })
    .await?
}

/// Set a changed file's note; an empty or missing note clears it.
#[tauri::command]
pub async fn set_skill_change_note(
    skill_id: String,
    path: String,
    note: Option<String>,
    store: State<'_, Arc<SkillStore>>,
) -> Result<ChangesReport, AppError> {
    let store = store.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let skill = find_skill(&store, &skill_id)?;
        match note.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
            Some(note) => change_notes::set_note(&store, &skill, &path, note),
            None => change_notes::clear_note(&store, &skill, &path),
        }
        .map_err(|err| AppError::invalid_input(format!("{err:#}")))
    })
    .await?
}

/// Write the skill's local changes as a patch to `output_path`.
#[tauri::command]
pub async fn export_skill_local_patch(
    skill_id: String,
    output_path: String,
    store: State<'_, Arc<SkillStore>>,
) -> Result<PatchExport, AppError> {
    let store = store.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let skill = find_skill(&store, &skill_id)?;
        let export = change_notes::export_patch(&store, &skill, store.proxy_url().as_deref())
            .map_err(internal)?;
        let output = PathBuf::from(&output_path);
        std::fs::write(&output, &export.patch)
            .map_err(|err| AppError::io(format!("failed to write {output_path}: {err}")))?;
        Ok(export)
    })
    .await?
}
