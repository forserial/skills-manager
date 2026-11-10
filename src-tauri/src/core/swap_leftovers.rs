//! Hidden folders an interrupted install or update leaves in the library, put
//! back or cleaned at startup (sm-69q.23).
//!
//! Installs and updates copy into `.X.staged-<uuid>` beside the skill folder
//! `X` and swap it in through `.X.backup-<uuid>` (sm-69q.20). Three things can
//! leave such a folder behind: a crash during the copy leaves the staged
//! folder; a crash between the swap's two renames leaves the old copy only as
//! the backup, with nothing at `X`, so the next reindex would drop `X`'s
//! record; and a backup the OS will not delete after a good swap (a locked
//! file) stays beside the new `X`.
//!
//! [`recover_at_startup`] runs before the startup reindex, under the repo lock:
//!
//! - **Put back.** The one backup of a missing `X` that has a record is renamed
//!   to `X`, so the reindex finds the folder. The record is the metadata file
//!   under `.skills-manager/skills/` (the database only stands in when the
//!   library has no complete metadata snapshot, as the reindex then does not
//!   run). It still describes the old copy: the swap updates the record only
//!   after both renames.
//! - **Removed.** A staged folder, and a backup beside a live `X`, once nothing
//!   under it has changed for [`STALE_AFTER`]. Both are copies nothing points
//!   at: an install registers its folder only after renaming it into place,
//!   and the swap meant to delete the backup.
//! - **Left, and listed** by `skills orphans` with its kind: several backups of
//!   one missing `X` (which one is current cannot be told), a backup of an `X`
//!   no record names, a folder changed within [`STALE_AFTER`], or one that
//!   could not be moved or removed. Nothing is guessed: a live `X` is never
//!   replaced, and a backup never overwrites anything.
//!
//! Why an age as well as the lock: updates stage and swap under the repo lock,
//! and so do the installs checked here, but the app and the CLI run side by
//! side and not every path that stages into the library has been shown to hold
//! the lock for the whole copy. A staged folder written to recently may be a
//! copy in progress in the other process, so it is left alone. Putting a backup
//! back needs no age: a swap racing it fails its second rename (`X` is there
//! again), reports the update as failed and keeps the old copy, which is the
//! copy put back.
//!
//! Only names this app makes are touched: a leading dot, the skill folder's
//! name, then `.staged-` or `.backup-` and a UUID. A user's own dot-folder is
//! never one of them.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::Serialize;

use super::central_repo;
use super::repo_lock::{self, RepoLock};
use super::skill_store::SkillStore;
use super::sync_metadata;

/// How long nothing under a staged folder, or a backup beside a live skill,
/// must have changed before it is removed. A copy into the library takes
/// seconds; an hour leaves a wide margin for a slow disk.
pub const STALE_AFTER: Duration = Duration::from_secs(60 * 60);

/// Which half of an install or update a hidden folder is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LeftoverKind {
    /// `.X.staged-<uuid>`: a copy being made, never registered.
    Staged,
    /// `.X.backup-<uuid>`: the copy an update set aside.
    Backup,
}

/// A hidden staged or backup folder in the library.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Leftover {
    pub path: PathBuf,
    pub kind: LeftoverKind,
    /// Where the skill folder it belongs to is (or was): `X` beside it.
    pub skill_dir: PathBuf,
}

/// What [`recover`] did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct RecoveryReport {
    /// Backups renamed back to their skill folder.
    pub restored: Vec<PathBuf>,
    /// Staged folders and backups removed.
    pub removed: Vec<PathBuf>,
    /// Left in place, with why. `skills orphans` lists them.
    pub kept: Vec<(PathBuf, String)>,
}

/// `Some((kind, X))` for a folder name this app makes beside skill `X`.
pub(crate) fn parse_name(name: &str) -> Option<(LeftoverKind, &str)> {
    let rest = name.strip_prefix('.')?;
    let (skill, tail) = rest.rsplit_once('.')?;
    let (kind, id) = if let Some(id) = tail.strip_prefix("staged-") {
        (LeftoverKind::Staged, id)
    } else if let Some(id) = tail.strip_prefix("backup-") {
        (LeftoverKind::Backup, id)
    } else {
        return None;
    };
    uuid::Uuid::try_parse(id).ok()?;
    (!skill.is_empty()).then_some((kind, skill))
}

/// For a walk over the library: a folder that is an install's or update's
/// hidden copy, never a skill of its own.
pub(crate) fn is_leftover_name(name: &std::ffi::OsStr) -> bool {
    name.to_str().is_some_and(|name| parse_name(name).is_some())
}

/// Every staged or backup folder in the library: at its root, and beside any
/// recorded skill that lives deeper. Read-only.
pub fn find(store: &SkillStore) -> Vec<Leftover> {
    find_in(store, &central_repo::skills_dir())
}

pub(crate) fn find_in(store: &SkillStore, root: &Path) -> Vec<Leftover> {
    scan(root, &Records::read(store, root))
}

/// Startup step, before the reindex (see the module docs). Never fails the
/// start: what it cannot do is logged and left for `skills orphans`.
pub fn recover_at_startup(store: &SkillStore) {
    recover_waiting(store, repo_lock::FOREGROUND_WAIT, SystemTime::now());
}

/// [`recover_at_startup`] with the lock wait and the clock given.
pub(crate) fn recover_waiting(store: &SkillStore, wait: Duration, now: SystemTime) -> Option<RecoveryReport> {
    let root = central_repo::skills_dir();
    // Every CLI command starts here: look first, so a library with nothing to
    // recover is not locked for it. One that appears after this look belongs
    // to an operation running now, which is not this step's to touch.
    if !root.is_dir() || find_in(store, &root).is_empty() {
        return None;
    }
    let _lock = match RepoLock::acquire_blocking("recover interrupted installs", wait) {
        Ok(lock) => lock,
        Err(err) => {
            note(format!("startup: left hidden install/update folders for the next start: {err:#}"));
            return None;
        }
    };
    let report = recover_unlocked(store, &root, now);
    log_report(&report);
    Some(report)
}

/// The work of [`recover_at_startup`]; the caller holds the repo lock.
pub(crate) fn recover_unlocked(store: &SkillStore, root: &Path, now: SystemTime) -> RecoveryReport {
    let records = Records::read(store, root);
    let mut report = RecoveryReport::default();

    let mut backups: BTreeMap<PathBuf, Vec<PathBuf>> = BTreeMap::new();
    for leftover in scan(root, &records) {
        match leftover.kind {
            LeftoverKind::Staged => remove_if_stale(&leftover.path, now, &mut report,
                "a copy an install or update was making"),
            LeftoverKind::Backup => backups.entry(leftover.skill_dir).or_default().push(leftover.path),
        }
    }

    for (skill_dir, found) in backups {
        if skill_dir.symlink_metadata().is_ok() {
            // The swap got as far as putting the new copy in place; this is
            // the old one it meant to delete.
            for backup in found {
                remove_if_stale(&backup, now, &mut report,
                    "an old copy an update set aside; the skill folder beside it is current");
            }
            continue;
        }
        let name = display_name(&skill_dir);
        if !records.names(root, &skill_dir) {
            for backup in found {
                report.kept.push((backup, format!(
                    "the old copy of '{name}', which is missing, but no skill record names \
                     '{name}'; rename it to '{name}' and adopt it with `skills orphans adopt`, \
                     or remove it"
                )));
            }
            continue;
        }
        if found.len() > 1 {
            for backup in found {
                report.kept.push((backup, format!(
                    "one of several old copies of '{name}', which is missing: which one is \
                     current cannot be told; rename the right one to '{name}' and remove the others"
                )));
            }
            continue;
        }
        let backup = found.into_iter().next().expect("one backup");
        match super::fail_points::hit("leftovers.restore")
            .and_then(|()| std::fs::rename(&backup, &skill_dir))
        {
            Ok(()) => report.restored.push(skill_dir),
            Err(err) => report.kept.push((backup, format!(
                "the only copy of '{name}', which is missing; it could not be renamed back \
                 ({err}), so the next start drops the skill's record unless it is renamed to \
                 '{name}' first"
            ))),
        }
    }
    report
}

fn remove_if_stale(path: &Path, now: SystemTime, report: &mut RecoveryReport, what: &str) {
    if !inactive_for(path, now).is_some_and(|idle| idle >= STALE_AFTER) {
        report.kept.push((path.to_path_buf(), format!(
            "{what}; changed within the last hour, so it may still be in use"
        )));
        return;
    }
    match super::fail_points::hit("leftovers.remove").and_then(|()| std::fs::remove_dir_all(path)) {
        Ok(()) => report.removed.push(path.to_path_buf()),
        Err(err) => report.kept.push((path.to_path_buf(), format!(
            "{what}; it could not be removed ({err})"
        ))),
    }
}

/// How long since anything under `dir` (itself included) was created or
/// changed. `None` when that cannot be told, or the clock says the future:
/// then it counts as in use.
fn inactive_for(dir: &Path, now: SystemTime) -> Option<Duration> {
    let mut newest = SystemTime::UNIX_EPOCH;
    for entry in walkdir::WalkDir::new(dir) {
        let meta = entry.ok()?.metadata().ok()?;
        newest = newest.max(meta.modified().ok()?);
        // A copied file can keep its source's modification time (Windows
        // does); its creation time is when the copy made it.
        if let Ok(created) = meta.created() {
            newest = newest.max(created);
        }
    }
    now.duration_since(newest).ok()
}

/// Folders to look in: the root, and the parent of every recorded skill.
fn scan(root: &Path, records: &Records) -> Vec<Leftover> {
    let mut dirs = vec![root.to_path_buf()];
    for parent in records.parents(root) {
        if !dirs.contains(&parent) {
            dirs.push(parent);
        }
    }
    let mut found = Vec::new();
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            // Not followed: a link is never one of ours.
            if !entry.file_type().is_ok_and(|t| t.is_dir()) {
                continue;
            }
            let name = entry.file_name();
            let Some((kind, skill)) = name.to_str().and_then(parse_name) else {
                continue;
            };
            found.push(Leftover {
                path: entry.path(),
                kind,
                skill_dir: dir.join(skill),
            });
        }
    }
    found.sort_by(|a, b| a.path.cmp(&b.path));
    found
}

/// The skill paths the library has records for, relative to its root and
/// folded as the reindex folds them.
struct Records {
    keys: HashSet<String>,
    relative: Vec<String>,
}

impl Records {
    fn read(store: &SkillStore, root: &Path) -> Self {
        let relative = if sync_metadata::has_complete_skill_snapshot() {
            metadata_paths()
        } else {
            database_paths(store, root)
        };
        Self {
            keys: relative.iter().map(|path| sync_metadata::path_key(path)).collect(),
            relative,
        }
    }

    fn names(&self, root: &Path, skill_dir: &Path) -> bool {
        relative_to(root, skill_dir).is_some_and(|rel| self.keys.contains(&sync_metadata::path_key(&rel)))
    }

    fn parents(&self, root: &Path) -> Vec<PathBuf> {
        self.relative
            .iter()
            .filter_map(|rel| rel.rsplit_once('/').map(|(parent, _)| root.join(parent)))
            .collect()
    }
}

/// `path` of every metadata file, as the reindex reads it. One that cannot be
/// read is skipped: it names no folder to put back.
fn metadata_paths() -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(sync_metadata::metadata_dir().join("skills")) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
        .filter_map(|entry| std::fs::read_to_string(entry.path()).ok())
        .filter_map(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
        .filter_map(|value| value.get("path")?.as_str().map(clean_relative))
        .filter(|path| !path.is_empty())
        .collect()
}

fn database_paths(store: &SkillStore, root: &Path) -> Vec<String> {
    store
        .get_all_skills()
        .unwrap_or_default()
        .iter()
        .filter_map(|skill| relative_to(root, Path::new(&skill.central_path)))
        .collect()
}

fn relative_to(root: &Path, path: &Path) -> Option<String> {
    let rel = path.strip_prefix(root).ok()?;
    let rel = rel
        .components()
        .map(|c| c.as_os_str().to_str())
        .collect::<Option<Vec<_>>>()?
        .join("/");
    (!rel.is_empty()).then_some(rel)
}

fn clean_relative(path: &str) -> String {
    path.split(['/', '\\'])
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("/")
}

fn display_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_default()
}

fn log_report(report: &RecoveryReport) {
    for path in &report.restored {
        note(format!("startup: put back {} from the copy an interrupted update set aside", path.display()));
    }
    for path in &report.removed {
        note(format!("startup: removed {}, left by an interrupted install or update", path.display()));
    }
    for (path, why) in &report.kept {
        note(format!("startup: left {}: {why}", path.display()));
    }
}

/// The app's logger is not up yet when this runs, so the message is also kept
/// for the log file the app writes once it is (`take_startup_errors`).
fn note(message: String) {
    log::warn!("{message}");
    central_repo::record_startup_error(message);
}

/// Why `skills orphans` lists a hidden folder, and what the owner can do. Read
/// without the lock, so it describes the folder as it is now.
pub fn describe(leftover: &Leftover, now: SystemTime) -> String {
    let name = display_name(&leftover.skill_dir);
    let recent = !inactive_for(&leftover.path, now).is_some_and(|idle| idle >= STALE_AFTER);
    match leftover.kind {
        LeftoverKind::Staged if recent => format!(
            "A copy an install or update of '{name}' was making. It may still be in use; the \
             next start after an hour without changes removes it."
        ),
        LeftoverKind::Staged => format!(
            "A copy an install or update of '{name}' was making and never finished. It is not a \
             skill; remove it (the next start does, unless a file in it is locked)."
        ),
        LeftoverKind::Backup if leftover.skill_dir.symlink_metadata().is_ok() => format!(
            "An old copy of '{name}' an update set aside; '{name}' beside it is current. The \
             next start after an hour without changes removes it, unless a file in it is locked; \
             you can remove it yourself."
        ),
        LeftoverKind::Backup => format!(
            "The old copy of '{name}', which is missing. The next start renames it back when it is \
             the only one and a skill record names '{name}'; otherwise rename the right copy to \
             '{name}' yourself (then `skills orphans adopt {name}` if it has no record)."
        ),
    }
}

#[cfg(test)]
mod tests;
