//! Taking a skill's folder out of the library before its record is dropped
//! (sm-69q.21).
//!
//! Deleting a skill used to ignore a failed `remove_dir_all` and drop the record
//! anyway. On Windows a locked or read-only file leaves part of the folder
//! behind, and a half-deleted folder with a `SKILL.md` and no record is an
//! orphan. So the folder is first *renamed* to a hidden sibling in the same
//! directory: a rename is one step that either happens or does not, and a hidden
//! name is never listed as a skill nor offered by `skills orphans`.
//!
//! - rename fails: nothing changed; the caller keeps the record and reports the
//!   skill as failed ([`kept_failure`]);
//! - rename works: the caller drops the record, then [`Retired::discard`]
//!   removes the moved folder best-effort. Whatever cannot be removed is logged
//!   and left under its hidden name, never under the skill's own.

use std::io;
use std::path::{Path, PathBuf};

use serde::Serialize;

use super::audit_log::AuditDraft;
use super::error::AppError;
use super::skill_store::SkillStore;

/// Infix of the hidden sibling's name: `.<folder>.deleting-<uuid>`.
const MARKER: &str = ".deleting-";

/// Why one skill was kept instead of deleted.
#[derive(Debug, Clone, Serialize)]
pub struct DeleteFailure {
    pub skill_id: String,
    pub reason: String,
}

/// A folder moved out of the library under a hidden name, or nothing when the
/// skill had no folder.
#[must_use = "a retired folder must be discarded or restored"]
pub struct Retired {
    original: PathBuf,
    moved: Option<PathBuf>,
}

/// Move `central` to a hidden sibling. A missing folder is not a failure.
pub fn retire(central: &Path) -> io::Result<Retired> {
    retire_with(central, |from, to| std::fs::rename(from, to))
}

fn retire_with(
    central: &Path,
    rename: impl FnOnce(&Path, &Path) -> io::Result<()>,
) -> io::Result<Retired> {
    // A dangling symlink does not "exist" but is still ours to take away.
    if central.symlink_metadata().is_err() {
        return Ok(Retired {
            original: central.to_path_buf(),
            moved: None,
        });
    }
    let name = central
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "skill folder has no name"))?;
    let parent = central
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "skill folder has no parent"))?;
    let moved = parent.join(format!(".{name}{MARKER}{}", uuid::Uuid::new_v4().simple()));

    #[cfg(test)]
    let rename = |from: &Path, to: &Path| -> io::Result<()> {
        if test_hooks::FAIL_RENAME.with(|f| f.get()) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "simulated: folder is locked",
            ));
        }
        rename(from, to)
    };

    rename(central, &moved)?;
    Ok(Retired {
        original: central.to_path_buf(),
        moved: Some(moved),
    })
}

impl Retired {
    /// The record is gone: remove the moved folder, best-effort. Anything left
    /// is logged with its path; it sits under a hidden name, not as a skill.
    pub fn discard(self) {
        let Some(moved) = self.moved else { return };
        if let Err(e) = std::fs::remove_dir_all(&moved) {
            if moved.symlink_metadata().is_ok() {
                log::warn!(
                    "Skill folder {} was moved to {} and could not be fully removed: {e}. \
                     It can be deleted by hand.",
                    self.original.display(),
                    moved.display()
                );
            }
        }
    }

    /// Dropping the record failed after the move: put the folder back so the
    /// skill is not left managed with no folder.
    pub fn restore(self) {
        let Some(moved) = self.moved else { return };
        if let Err(e) = std::fs::rename(&moved, &self.original) {
            log::error!(
                "Could not move {} back to {}: {e}",
                moved.display(),
                self.original.display()
            );
        }
    }
}

/// Audit the refusal and build what the callers show. `err` is the rename error.
pub fn kept_failure(
    store: &SkillStore,
    skill_id: &str,
    skill_name: &str,
    err: &io::Error,
) -> DeleteFailure {
    let reason = format!(
        "the skill's folder could not be removed from the library ({err}); \
         the skill was kept. Close anything using it and try again"
    );
    store.log_audit(
        AuditDraft::new("remove")
            .skill(skill_id.to_string(), skill_name.to_string())
            .fail(reason.clone()),
    );
    DeleteFailure {
        skill_id: skill_id.to_string(),
        reason,
    }
}

/// The error for a single-skill delete that removed nothing: the reason when
/// the skill was kept, "not found" otherwise.
pub fn single_delete_error(skill_id: &str, failures: &[DeleteFailure]) -> AppError {
    match failures.iter().find(|f| f.skill_id == skill_id) {
        Some(f) => AppError::io(&f.reason),
        None => AppError::not_found("Skill not found"),
    }
}

#[cfg(test)]
pub(crate) mod test_hooks {
    use std::cell::Cell;

    thread_local! {
        /// Makes the next renames on this thread fail, standing in for a locked folder.
        pub static FAIL_RENAME: Cell<bool> = const { Cell::new(false) };
    }

    /// Run `f` with folder renames failing.
    pub fn with_failing_rename<T>(f: impl FnOnce() -> T) -> T {
        struct Reset;
        impl Drop for Reset {
            fn drop(&mut self) {
                FAIL_RENAME.with(|c| c.set(false));
            }
        }
        FAIL_RENAME.with(|c| c.set(true));
        let _reset = Reset;
        f()
    }
}

#[cfg(test)]
mod tests;
