//! Keep the database in step with the library on disk (sm-69q.18, sm-69q.22).
//!
//! The metadata rewrite (`sync_metadata::write_all_from_db_unlocked`) deletes
//! every record file the database does not know, and the auto-backup commits
//! that and sync spreads it. So whenever the disk changes, the database must
//! be reindexed before anything writes the metadata again. This module holds
//! the pieces that make that hold:
//!
//! - The CLI's `git pull`, `git restore` and `git clone` change the disk and
//!   reindex under one repo lock ([`pull_and_reindex`], [`restore_and_reindex`],
//!   [`clone_and_reindex`]), as the app's own commands already do. Before, the
//!   CLI pull reindexed under a second lock, and restore and clone did not
//!   reindex at all, so the running app's next write deleted the records of
//!   the skills they brought in.
//! - The reindex writes the database in one transaction ([`ReindexTransaction`]),
//!   so a reindex that fails leaves the database as it was, not half-updated.
//! - A failed reindex still leaves the database behind the disk. It is recorded
//!   in a marker beside the database ([`record_reindex_outcome`]), and while the
//!   marker is there the metadata rewrite writes records but deletes none
//!   ([`deletions_allowed`]). The next reindex that succeeds removes it. A
//!   deliberate delete is therefore only deferred, never lost: its record file
//!   goes on the first rewrite after a good reindex, and on other devices the
//!   reindex drops a record whose folder is gone.
//! - The reindex no longer drops a record just because `is_dir()` is false:
//!   a dangling link (a drive not mounted yet) keeps its record, and a folder
//!   whose name differs from the record only by case is found on a
//!   case-sensitive device ([`locate_skill_dir`]).
//!
//! Not covered: a `git pull` or checkout run by hand in the library, or a
//! folder-sync tool, changes the disk without telling the app; the next write
//! can still drop records then. The CLI's next start (it reindexes first) or
//! the app's next start heals the database only if no write ran in between.
//! `skills orphans` (sm-69q.16) recovers folders that lost their record.

use std::path::{Path, PathBuf};

use anyhow::Result;

use super::central_repo;
use super::git_backup;
use super::merge;
use super::repo_lock::RepoLock;
use super::skill_store::{SkillRecord, SkillStore};
use super::sync_metadata::{self, SkillMetaFile};

// ── CLI git operations: change the disk and reindex under one lock ──

fn reconcile(store: &SkillStore) -> Result<()> {
    crate::commands::git_backup::reconcile_skills_index_unlocked(store)
}

/// CLI `git pull`: the merge and the reindex under the one lock, as the app's
/// `git_backup_pull` does.
pub fn pull_and_reindex(
    store: &SkillStore,
    skills_dir: &Path,
    device_name: &str,
) -> Result<merge::MergeSummary> {
    let _lock = RepoLock::acquire_foreground("git pull")?;
    let _ = git_backup::configure_device_identity(skills_dir, device_name);
    let summary = merge::gated_pull_unlocked(store, skills_dir)?;
    reconcile(store)?;
    Ok(summary)
}

/// CLI `git restore`: as the app's `git_backup_restore_version`. Returns the
/// safety-point tag.
pub fn restore_and_reindex(store: &SkillStore, skills_dir: &Path, tag: &str) -> Result<String> {
    let _lock = RepoLock::acquire_foreground("git restore snapshot")?;
    let safety_tag = git_backup::restore_snapshot_version_unlocked(skills_dir, tag)?;
    reconcile(store)?;
    Ok(safety_tag)
}

/// CLI `git clone`: as the app's `git_backup_clone`. `strict` refuses a
/// populated target that is not a repository (`git_backup::clone_into_strict`).
pub fn clone_and_reindex(store: &SkillStore, skills_dir: &Path, url: &str, strict: bool) -> Result<()> {
    let _lock = RepoLock::acquire_foreground("git clone")?;
    if strict {
        git_backup::ensure_clean_clone_target(skills_dir)?;
    }
    git_backup::clone_into_unlocked(skills_dir, url, &[])?;
    reconcile(store)
}

// ── a failed reindex: defer deletions until one succeeds ──

/// Beside the database, never inside the library: it describes this device's
/// database, so it must not be backed up or synced.
const MARKER: &str = ".skills-manager.reindex-failed";

fn marker_path() -> PathBuf {
    central_repo::base_dir().join(MARKER)
}

/// Called with every reindex result.
pub(crate) fn record_reindex_outcome(result: &Result<()>) {
    let path = marker_path();
    match result {
        Ok(()) => {
            if path.exists() {
                if let Err(err) = std::fs::remove_file(&path) {
                    log::warn!("reindex succeeded but {} stays: {err}", path.display());
                }
            }
        }
        Err(err) => {
            let note = format!(
                "The last reindex failed, so the database may not match the library on disk.\n\
                 Metadata rewrites keep record files until a reindex succeeds.\n\
                 at: {}\nerror: {err:#}\n",
                chrono::Utc::now().to_rfc3339()
            );
            if let Err(write_err) = std::fs::write(&path, note) {
                log::error!(
                    "reindex failed and {} cannot be written ({write_err}); \
                     the next metadata rewrite may delete records: {err:#}",
                    path.display()
                );
            }
        }
    }
}

/// Whether the metadata rewrite may delete record files the database does not
/// know: not while the last reindex failed.
pub(crate) fn deletions_allowed() -> bool {
    let path = marker_path();
    if !path.exists() {
        return true;
    }
    log::warn!(
        "metadata rewrite: keeping record files the database does not know, \
         because the last reindex failed (see {})",
        path.display()
    );
    false
}

// ── one transaction for the reindex's writes ──

const SAVEPOINT: &str = "sm_reindex";

/// The reindex's writes as one SQLite transaction: rolled back on drop unless
/// committed, so an early `?` leaves the database as it was. A savepoint, so
/// the store methods that open their own (`replace_scenarios_from_metadata`
/// and `replace_scenario_memberships_from_metadata`) nest inside it.
///
/// The transaction is on the store's one connection, which other threads share;
/// keep it short (the reindex reads and hashes every skill before it begins).
pub(crate) struct ReindexTransaction<'a> {
    store: &'a SkillStore,
    open: bool,
}

impl<'a> ReindexTransaction<'a> {
    pub(crate) fn begin(store: &'a SkillStore) -> Result<Self> {
        store
            .conn
            .lock()
            .unwrap()
            .execute_batch(&format!("SAVEPOINT {SAVEPOINT}"))?;
        Ok(Self { store, open: true })
    }

    pub(crate) fn commit(mut self) -> Result<()> {
        self.store
            .conn
            .lock()
            .unwrap()
            .execute_batch(&format!("RELEASE {SAVEPOINT}"))?;
        self.open = false;
        Ok(())
    }
}

impl Drop for ReindexTransaction<'_> {
    fn drop(&mut self) {
        if !self.open {
            return;
        }
        let conn = self.store.conn.lock().unwrap_or_else(|e| e.into_inner());
        if let Err(err) =
            conn.execute_batch(&format!("ROLLBACK TO {SAVEPOINT}; RELEASE {SAVEPOINT}"))
        {
            log::error!("reindex: rolling back its writes failed: {err}");
        }
    }
}

// ── where a record's folder is ──

pub(crate) enum SkillDir {
    /// The folder, at the record's path or at one that differs only by case.
    Found(PathBuf),
    /// A link whose target cannot be reached (a drive not mounted yet). The
    /// record is kept as it was; the link is the skill's place.
    Unreachable(PathBuf),
    /// Nothing there: the skill is gone.
    Gone,
}

/// Find the folder of the skill recorded at `rel` (a `/`-separated path
/// under `skills_root`).
pub(crate) fn locate_skill_dir(skills_root: &Path, rel: &str) -> SkillDir {
    let exact = skills_root.join(rel);
    let path = if exact.is_dir() || exists_no_follow(&exact) {
        exact
    } else {
        match case_folded(skills_root, rel) {
            Some(found) => found,
            None => return SkillDir::Gone,
        }
    };
    if path.is_dir() {
        SkillDir::Found(path)
    } else if is_dangling_link(&path) {
        SkillDir::Unreachable(path)
    } else {
        SkillDir::Gone
    }
}

fn exists_no_follow(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok()
}

fn is_dangling_link(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink())
        && std::fs::metadata(path).is_err()
}

/// `rel` resolved one component at a time, taking the exact name when it is
/// there and otherwise the one entry whose name folds to the same key the
/// record's `path_key` is built with. `None` when a component has no match or
/// more than one (two names that differ only by case on a case-sensitive
/// device: which one is the skill's cannot be told).
///
/// The match cannot belong to another record: `path_key`s are unique across
/// the metadata (`ensure_unique_path_keys`), and this one is the record's own.
fn case_folded(skills_root: &Path, rel: &str) -> Option<PathBuf> {
    let mut path = skills_root.to_path_buf();
    for part in rel.split('/') {
        let exact = path.join(part);
        if exists_no_follow(&exact) {
            path = exact;
            continue;
        }
        let key = sync_metadata::path_key(part);
        let mut matches = std::fs::read_dir(&path)
            .ok()?
            .flatten()
            .filter(|entry| sync_metadata::path_key(&entry.file_name().to_string_lossy()) == key);
        let found = matches.next()?;
        if matches.next().is_some() {
            return None;
        }
        path = found.path();
    }
    Some(path)
}

/// The row for a skill whose folder is a dangling link: the previous row with
/// what the record file says, or, with no previous row, what can be known
/// without reading the folder.
pub(crate) fn unreachable_record(
    meta: &SkillMetaFile,
    link: &Path,
    previous: Option<&SkillRecord>,
    now: i64,
) -> SkillRecord {
    let source_ref = if matches!(meta.source.source_type.as_str(), "import" | "local") {
        previous.and_then(|s| s.source_ref.clone())
    } else {
        meta.source.ref_.clone()
    };
    let from_meta = |base: SkillRecord| SkillRecord {
        source_type: meta.source.source_type.clone(),
        source_ref: source_ref.clone(),
        source_subpath: meta.source.subpath.clone(),
        source_branch: meta.source.branch.clone(),
        central_path: link.to_string_lossy().to_string(),
        enabled: meta.enabled,
        ..base
    };
    match previous {
        Some(prev) => from_meta(prev.clone()),
        None => from_meta(SkillRecord {
            id: meta.skill_id.clone(),
            name: link
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "unknown-skill".to_string()),
            description: None,
            source_type: String::new(),
            source_ref: None,
            source_ref_resolved: None,
            source_subpath: None,
            source_branch: None,
            source_revision: None,
            remote_revision: None,
            central_path: String::new(),
            content_hash: None,
            enabled: true,
            created_at: now,
            updated_at: now,
            status: "ok".to_string(),
            update_status: "unknown".to_string(),
            last_checked_at: None,
            last_check_error: None,
        }),
    }
}

#[cfg(test)]
mod tests;
