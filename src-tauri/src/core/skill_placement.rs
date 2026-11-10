//! Putting a skill folder into the library together with its record
//! (sm-69q.19, sm-69q.20).
//!
//! The startup reindex rebuilds the database from the metadata files alone,
//! so a folder whose metadata file was never written is dropped from the
//! library (an orphan, recoverable with `skills orphans`), and a record whose
//! folder is missing is dropped too. The helpers here keep the two together
//! on every failure an install can report, and keep the crash window between
//! them as short as one small file write:
//!
//! - [`place_copy`]: the installer copies into a staged `.name.staged-<uuid>`
//!   folder beside the destination (ignored by the orphan finder, as update
//!   staging already is) and renames it into place only once the copy and its
//!   hash succeeded. A failed copy leaves nothing; a crash during the copy
//!   leaves only the hidden staged folder, never a half-written skill.
//! - [`write_skill_metadata_unlocked`]: one skill's metadata file, written
//!   right after its row, before the slower full rewrite that fsyncs every file.
//! - [`undo_new_install`]: when registering fails, the row, metadata file and
//!   folder that this install created are removed again, and nothing that was
//!   there before is touched.
//!
//! What is left open, and why, is listed on sm-69q.20.

use anyhow::Result;
use std::path::Path;

use super::fail_points;
use super::skill_store::SkillStore;
use super::sync_metadata;
use crate::commands::skills as skill_cmd;

/// Run `fill` on a fresh staged folder beside `destination`, then move the
/// result into `destination`, replacing what is there. Returns `fill`'s value
/// and whether `destination` was created by this call (nothing was there).
///
/// On any failure the staged folder is removed and `destination` is left as
/// it was.
pub(crate) fn place_copy<T>(
    destination: &Path,
    fill: impl FnOnce(&Path) -> Result<T>,
) -> Result<(T, bool)> {
    let existing = destination.symlink_metadata().ok();
    let staged = skill_cmd::staged_path_for(&destination.to_string_lossy());

    let filled = fill(&staged).and_then(|value| {
        fail_points::hit("place.after_copy")?;
        Ok(value)
    });
    let value = match filled {
        Ok(value) => value,
        Err(err) => {
            remove_quietly(&staged);
            return Err(err);
        }
    };

    match existing {
        None => {
            let renamed = fail_points::hit("place.rename")
                .and_then(|()| std::fs::rename(&staged, destination));
            if let Err(err) = renamed {
                remove_quietly(&staged);
                return Err(err.into());
            }
            Ok((value, true))
        }
        Some(meta) if meta.file_type().is_symlink() => {
            // A link where the skill belongs is replaced by a real copy, as the
            // installer always did. Only the link is removed, not its target.
            if let Err(err) = remove_link(destination) {
                remove_quietly(&staged);
                return Err(err.into());
            }
            if let Err(err) = std::fs::rename(&staged, destination) {
                remove_quietly(&staged);
                return Err(err.into());
            }
            Ok((value, false))
        }
        Some(_) => {
            // Restores the old folder and removes the staged one on failure.
            skill_cmd::swap_skill_directory(&staged, destination)?;
            Ok((value, false))
        }
    }
}

/// Write `skill_id`'s metadata file. When the library has no complete
/// metadata snapshot yet, a single file would make one, and the next reindex
/// would then drop every other skill; the full rewrite runs instead.
pub(crate) fn write_skill_metadata_unlocked(store: &SkillStore, skill_id: &str) -> Result<()> {
    fail_points::hit("metadata.write")?;
    if sync_metadata::has_complete_skill_snapshot() {
        sync_metadata::ensure_skill_metadata_unlocked(store, skill_id)
    } else {
        sync_metadata::write_all_from_db_unlocked(store)
    }
}

/// Undo an install whose registration failed: remove the row it inserted
/// (`inserted_id`, with its metadata file) and the folder it created. A row
/// or folder that was there before the install is kept.
pub(crate) fn undo_new_install(
    store: &SkillStore,
    inserted_id: Option<&str>,
    folder: &Path,
    created_folder: bool,
) {
    if let Some(id) = inserted_id {
        if let Err(err) = store.delete_skill(id) {
            log::warn!("could not remove the record of a failed install {id}: {err:#}");
        }
        let file = sync_metadata::metadata_dir()
            .join("skills")
            .join(format!("{id}.json"));
        if file.exists() {
            if let Err(err) = std::fs::remove_file(&file) {
                log::warn!("could not remove {}: {err:#}", file.display());
            }
        }
    }
    if created_folder {
        if let Err(err) = std::fs::remove_dir_all(folder) {
            log::warn!(
                "could not remove {} after a failed install: {err:#}",
                folder.display()
            );
        }
    }
}

/// Insert `record` for a folder the installer just placed, and write its
/// metadata file. On failure the row and, if this install created it, the
/// folder are removed again. For the import paths that build their own
/// record instead of going through `store_installed_skill_unlocked`.
pub(crate) fn register_new_skill_unlocked(
    store: &SkillStore,
    record: &super::skill_store::SkillRecord,
    created_folder: bool,
) -> Result<()> {
    let folder = Path::new(&record.central_path);
    if let Err(err) = store.insert_skill(record) {
        undo_new_install(store, None, folder, created_folder);
        return Err(err);
    }
    if let Err(err) = write_skill_metadata_unlocked(store, &record.id) {
        undo_new_install(store, Some(&record.id), folder, created_folder);
        return Err(err);
    }
    Ok(())
}

fn remove_quietly(path: &Path) {
    if path.symlink_metadata().is_ok() {
        if let Err(err) = std::fs::remove_dir_all(path) {
            log::warn!("could not remove {}: {err:#}", path.display());
        }
    }
}

fn remove_link(path: &Path) -> std::io::Result<()> {
    // A directory link (Windows junction or symlink) is removed with
    // remove_dir, a file link with remove_file; neither touches the target.
    std::fs::remove_file(path).or_else(|_| std::fs::remove_dir(path))
}
