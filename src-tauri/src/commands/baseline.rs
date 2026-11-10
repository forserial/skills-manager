//! Tauri commands for local-change detection (sm-69q.3).
//!
//! Thin wrappers over `core::baseline`; every call runs on a blocking thread
//! because it hashes the skill's files, and the diff may fetch the baseline
//! revision over the network.

use std::sync::Arc;

use serde::Serialize;
use tauri::State;

use crate::core::baseline::{self, LocalChanges, LocalDiff, LocalState};
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

/// Which files of a library skill differ from its baseline.
#[tauri::command]
pub async fn get_skill_local_changes(
    skill_id: String,
    store: State<'_, Arc<SkillStore>>,
) -> Result<LocalChanges, AppError> {
    let store = store.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let skill = find_skill(&store, &skill_id)?;
        baseline::local_changes(&store, &skill).map_err(internal)
    })
    .await?
}

/// What a skill card shows: its state and how many files changed.
#[derive(Debug, Serialize)]
pub struct LocalChangeMarker {
    pub skill_id: String,
    pub state: LocalState,
    pub change_count: Option<usize>,
}

/// One marker per git or skills.sh skill whose state is not clean. Local
/// skills and clean skills are left out; so is a skill whose library copy
/// could not be read.
#[tauri::command]
pub async fn get_local_change_markers(
    store: State<'_, Arc<SkillStore>>,
) -> Result<Vec<LocalChangeMarker>, AppError> {
    let store = store.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let skills = store.get_all_skills().map_err(AppError::db)?;
        let changes = baseline::local_changes_for(&store, &skills).map_err(internal)?;
        let mut markers: Vec<LocalChangeMarker> = changes
            .into_values()
            .filter(|c| !matches!(c.state, LocalState::Clean | LocalState::Untracked))
            .map(|c| LocalChangeMarker {
                change_count: c.change_count(),
                state: c.state,
                skill_id: c.skill_id,
            })
            .collect();
        markers.sort_by(|a, b| a.skill_id.cmp(&b.skill_id));
        Ok(markers)
    })
    .await?
}

/// The library copy of a skill against its baseline content.
#[tauri::command]
pub async fn get_skill_local_diff(
    skill_id: String,
    store: State<'_, Arc<SkillStore>>,
) -> Result<LocalDiff, AppError> {
    let store = store.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let skill = find_skill(&store, &skill_id)?;
        baseline::local_diff(&store, &skill, store.proxy_url().as_deref()).map_err(internal)
    })
    .await?
}
