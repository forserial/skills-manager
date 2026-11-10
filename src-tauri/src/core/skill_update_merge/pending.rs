//! The pending-merge record: an update held over conflicts, the target
//! revision, and the choices made so far, one file at a time.
//!
//! Where it lives: `base_dir()/pending-merges/<skill_id>/`, beside the
//! database and the cache and never inside the library. It is device-local
//! on purpose:
//! - it describes this device's library copy against a tree this device
//!   fetched, so syncing it would hand another device choices about content
//!   it may not have;
//! - nothing in it may reach the library's Git backup, the reindex or the
//!   object merge (a subfolder of the metadata directory loses data there,
//!   design §11.1);
//! - it is not a cache: a file supplied with `--file` exists only here, so it
//!   stays out of `cache_dir()`.
//!
//! Losing it is always safe: the next update holds again and asks again.
//!
//! Layout: `merge.json` (this record), `versions/<key>/` holding each
//! conflicted file as `<stem>.mine<ext>`, `<stem>.theirs<ext>` and
//! `<stem>.base<ext>` (when the base is known) for "open both versions", and
//! `resolved/<key>` holding content supplied with `--file`.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

use super::MergeConflict;
use crate::core::central_repo;
use crate::core::fork_files::{self, ForkConcern};
use crate::core::skill_store::SkillRecord;

/// Which content a conflicted file takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Take {
    /// The library's copy ("keep mine"). For a file deleted locally, it
    /// stays deleted.
    Local,
    /// Upstream's copy ("take theirs"). For a file upstream deleted, it is
    /// deleted.
    Upstream,
    /// Content the person supplied, kept in this record.
    File,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Resolution {
    pub take: Take,
    /// Unix seconds.
    pub resolved_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingConflict {
    #[serde(flatten)]
    pub conflict: MergeConflict,
    #[serde(default)]
    pub resolution: Option<Resolution>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingMerge {
    pub skill_id: String,
    pub skill_name: String,
    /// The upstream revision the held update would move to.
    pub target_revision: String,
    /// The baseline the conflicts were computed against. A record for
    /// another baseline is stale and ignored.
    pub baseline_revision: String,
    pub created_at: i64,
    pub updated_at: i64,
    /// Paths that merged cleanly when the update was held.
    #[serde(default)]
    pub merged: Vec<String>,
    pub conflicts: Vec<PendingConflict>,
}

impl PendingMerge {
    pub fn unresolved(&self) -> Vec<&PendingConflict> {
        self.conflicts
            .iter()
            .filter(|c| c.resolution.is_none())
            .collect()
    }

    /// The recorded choice for `conflict`, if one was made for exactly this
    /// conflict: same path, and the library and upstream copies are still
    /// the ones the person decided about.
    pub fn resolution_for(
        &self,
        baseline_revision: &str,
        conflict: &MergeConflict,
    ) -> Option<Take> {
        if self.baseline_revision != baseline_revision {
            return None;
        }
        self.conflicts
            .iter()
            .find(|c| c.conflict.same_question(conflict))
            .and_then(|c| c.resolution.as_ref())
            .map(|r| r.take)
    }
}

pub(super) fn root() -> PathBuf {
    central_repo::base_dir().join("pending-merges")
}

/// The record directory of `skill_id`. The id is validated the way fork
/// files are, before anything is named after it.
pub fn dir_for(skill_id: &str) -> Result<PathBuf> {
    fork_files::path_for(ForkConcern::Baseline, skill_id)?;
    Ok(root().join(skill_id))
}

fn key_for(path: &str) -> String {
    hex::encode(&Sha256::digest(path.as_bytes())[..8])
}

/// The folder holding the versions of one conflicted file.
pub fn versions_dir(skill_id: &str, path: &str) -> Result<PathBuf> {
    Ok(dir_for(skill_id)?.join("versions").join(key_for(path)))
}

fn resolved_file(skill_id: &str, path: &str) -> Result<PathBuf> {
    Ok(dir_for(skill_id)?.join("resolved").join(key_for(path)))
}

/// `SKILL.md` → `SKILL.mine.md`; `Makefile` → `Makefile.mine`.
fn version_name(path: &str, label: &str) -> String {
    let name = path.rsplit('/').next().unwrap_or(path);
    match name.rfind('.') {
        Some(dot) if dot > 0 => format!("{}.{label}{}", &name[..dot], &name[dot..]),
        _ => format!("{name}.{label}"),
    }
}

pub fn load(skill_id: &str) -> Result<Option<PendingMerge>> {
    let path = dir_for(skill_id)?.join("merge.json");
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err).with_context(|| format!("failed to read {}", path.display())),
    };
    let record: PendingMerge = serde_json::from_slice(&bytes)
        .with_context(|| format!("{} is not a pending merge", path.display()))?;
    if record.skill_id != skill_id {
        bail!(
            "{} names {}, not {skill_id}",
            path.display(),
            record.skill_id
        );
    }
    Ok(Some(record))
}

/// Remove the record and everything it holds. `true` when there was one.
pub fn remove(skill_id: &str) -> Result<bool> {
    let dir = dir_for(skill_id)?;
    if !dir.exists() {
        return Ok(false);
    }
    std::fs::remove_dir_all(&dir).with_context(|| format!("failed to remove {}", dir.display()))?;
    Ok(true)
}

fn save(record: &PendingMerge) -> Result<()> {
    let dir = dir_for(&record.skill_id)?;
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("merge.json");
    let tmp = dir.join(format!("merge.json.tmp.{}", uuid::Uuid::new_v4()));
    std::fs::write(&tmp, serde_json::to_vec_pretty(record)?)?;
    std::fs::rename(&tmp, &path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })?;
    Ok(())
}

/// The three versions of one conflicted file, `None` where it is absent or
/// (for the base) unknown.
pub struct Versions {
    pub base: Option<Vec<u8>>,
    pub mine: Option<Vec<u8>>,
    pub theirs: Option<Vec<u8>>,
}

/// Record a held merge. Choices already made for the same questions are
/// kept; the rest are dropped with the old versions.
pub(crate) fn save_hold(
    skill: &SkillRecord,
    target_revision: &str,
    baseline_revision: &str,
    merged: &[String],
    conflicts: &[MergeConflict],
    versions: impl Fn(&MergeConflict) -> Result<Versions>,
) -> Result<PendingMerge> {
    let previous = load(&skill.id).ok().flatten();
    let now = chrono::Utc::now().timestamp();
    let record = PendingMerge {
        skill_id: skill.id.clone(),
        skill_name: skill.name.clone(),
        target_revision: target_revision.to_string(),
        baseline_revision: baseline_revision.to_string(),
        created_at: previous
            .as_ref()
            .filter(|p| p.baseline_revision == baseline_revision)
            .map_or(now, |p| p.created_at),
        updated_at: now,
        merged: merged.to_vec(),
        conflicts: conflicts
            .iter()
            .map(|conflict| PendingConflict {
                conflict: conflict.clone(),
                resolution: previous.as_ref().and_then(|p| {
                    p.resolution_for(baseline_revision, conflict)
                        .map(|take| Resolution {
                            take,
                            resolved_at: now,
                        })
                }),
            })
            .collect(),
    };

    let dir = dir_for(&skill.id)?;
    let versions_root = dir.join("versions");
    let _ = std::fs::remove_dir_all(&versions_root);
    for conflict in conflicts {
        let folder = versions_dir(&skill.id, &conflict.path)?;
        std::fs::create_dir_all(&folder)?;
        let v = versions(conflict)?;
        for (label, content) in [("base", &v.base), ("mine", &v.mine), ("theirs", &v.theirs)] {
            if let Some(content) = content {
                std::fs::write(folder.join(version_name(&conflict.path, label)), content)?;
            }
        }
    }
    // Supplied files for questions that no longer stand go too.
    let resolved_root = dir.join("resolved");
    if resolved_root.is_dir() {
        for entry in std::fs::read_dir(&resolved_root)?.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            let keep = record.conflicts.iter().any(|c| {
                key_for(&c.conflict.path) == name
                    && c.resolution.as_ref().is_some_and(|r| r.take == Take::File)
            });
            if !keep {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
    save(&record)?;
    Ok(record)
}

/// Conflict markers in supplied content: a line starting `<<<<<<< ` and one
/// starting `>>>>>>> `. A setext heading's `=======` alone is fine.
fn has_conflict_markers(content: &[u8]) -> bool {
    let text = String::from_utf8_lossy(content);
    let starts = |marker: &str| text.lines().any(|line| line.starts_with(marker));
    starts("<<<<<<<") && starts(">>>>>>>")
}

/// Record a choice for one conflicted file of the pending merge.
pub fn resolve(
    skill_id: &str,
    path: &str,
    take: Take,
    file: Option<&Path>,
) -> Result<PendingMerge> {
    let mut record = load(skill_id)?
        .with_context(|| format!("no pending merge for {skill_id}: run `skills update` first"))?;
    let path = path.replace('\\', "/");
    let Some(index) = record
        .conflicts
        .iter()
        .position(|c| c.conflict.path == path)
    else {
        let paths: Vec<&str> = record
            .conflicts
            .iter()
            .map(|c| c.conflict.path.as_str())
            .collect();
        bail!(
            "{path} is not a conflict of the pending merge for {skill_id} (conflicts: {})",
            paths.join(", ")
        );
    };
    let target = resolved_file(skill_id, &path)?;
    match (take, file) {
        (Take::File, Some(source)) => {
            let content = std::fs::read(source)
                .with_context(|| format!("failed to read {}", source.display()))?;
            if has_conflict_markers(&content) {
                bail!(
                    "{} still has conflict markers; resolve them first",
                    source.display()
                );
            }
            std::fs::create_dir_all(target.parent().context("resolved file has no parent")?)?;
            std::fs::write(&target, content)?;
        }
        (Take::File, None) => bail!("--file needs a path to the resolved content"),
        (_, Some(_)) => bail!("--take and --file cannot be combined"),
        (_, None) => {
            let _ = std::fs::remove_file(&target);
        }
    }
    record.conflicts[index].resolution = Some(Resolution {
        take,
        resolved_at: chrono::Utc::now().timestamp(),
    });
    record.updated_at = chrono::Utc::now().timestamp();
    save(&record)?;
    Ok(record)
}

/// The content supplied with `--file` for `path`. Only [`resolve`] writes it.
pub(crate) fn supplied_content(skill_id: &str, path: &str) -> Result<Vec<u8>> {
    let file = resolved_file(skill_id, path)?;
    std::fs::read(&file).with_context(|| {
        format!(
            "the resolved content for {path} is missing from {}; resolve it again",
            file.display()
        )
    })
}

#[cfg(test)]
pub(crate) fn version_file_name(path: &str, label: &str) -> String {
    version_name(path, label)
}
