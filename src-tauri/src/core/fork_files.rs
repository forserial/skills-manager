//! Fork-owned per-skill state, stored as flat JSON files in the metadata root.
//!
//! One file per skill per concern: `.skills-manager/fork-<concern>.<skill_id>.json`.
//! The layout was chosen in the sm-69q spike (epic design §11.1): flat files
//! in the metadata root are residual blobs to the object merge, merged
//! whole-file three-way with newest-commit-wins, and they survive reindex,
//! `write_all_from_db`, backup and clone on both this build and upstream's.
//! A subfolder loses data in a three-way merge, and anything under `skills/`
//! breaks sync, so neither is ever used. One file per concern keeps a pin set
//! on one device and a note added on another from overwriting each other.
//!
//! Writes use the same temp-file pattern as `sync_metadata` (`*.json.tmp.<uuid>`)
//! so the existing cleanup (`cleanup_temporary_files`, the commit-time
//! `remove_tmp_metadata_files` and the merge junk filter) handles leftovers.
//!
//! Files whose concern this build does not know are never touched, so a newer
//! build's concerns survive an older build.

use anyhow::{Result, bail};
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::collections::HashSet;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use super::repo_lock::RepoLock;
use super::skill_store::SkillStore;
use super::sync_metadata;

const FILE_PREFIX: &str = "fork-";
const FILE_SUFFIX: &str = ".json";

/// A kind of fork-owned per-skill state. Each maps to its own file stem.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ForkConcern {
    Baseline,
    Pin,
    Watch,
    Notes,
    Override,
}

impl ForkConcern {
    pub const ALL: [ForkConcern; 5] = [
        ForkConcern::Baseline,
        ForkConcern::Pin,
        ForkConcern::Watch,
        ForkConcern::Notes,
        ForkConcern::Override,
    ];

    /// Fixed file stem. Never rename one: it is the on-disk and synced name.
    pub fn stem(self) -> &'static str {
        match self {
            ForkConcern::Baseline => "fork-baseline",
            ForkConcern::Pin => "fork-pin",
            ForkConcern::Watch => "fork-watch",
            ForkConcern::Notes => "fork-notes",
            ForkConcern::Override => "fork-override",
        }
    }

    fn from_stem(stem: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.stem() == stem)
    }
}

/// `.skills-manager/<stem>.<skill_id>.json`, flat in the metadata root.
pub fn path_for(concern: ForkConcern, skill_id: &str) -> Result<PathBuf> {
    validate_skill_id(skill_id)?;
    Ok(sync_metadata::metadata_dir().join(format!("{}.{skill_id}{FILE_SUFFIX}", concern.stem())))
}

/// Read one concern's state for a skill. A missing file is `Ok(None)`.
pub fn read<T: DeserializeOwned>(concern: ForkConcern, skill_id: &str) -> Result<Option<T>> {
    let path = path_for(concern, skill_id)?;
    let raw = match fs::read(&path) {
        Ok(raw) => raw,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err.into()),
    };
    let value = serde_json::from_slice(&raw)
        .map_err(|err| anyhow::anyhow!("invalid fork file {}: {err}", path.display()))?;
    Ok(Some(value))
}

/// Write one concern's state for a skill, taking the repo lock briefly.
pub fn write<T: Serialize>(concern: ForkConcern, skill_id: &str, value: &T) -> Result<()> {
    let _lock = RepoLock::acquire_foreground("write fork file")?;
    write_unlocked(concern, skill_id, value)
}

/// [`write`] for callers that already hold the repo lock.
pub fn write_unlocked<T: Serialize>(concern: ForkConcern, skill_id: &str, value: &T) -> Result<()> {
    atomic_write_json(&path_for(concern, skill_id)?, value)
}

/// Remove one concern's state for a skill. Missing is not an error.
pub fn remove(concern: ForkConcern, skill_id: &str) -> Result<()> {
    let _lock = RepoLock::acquire_foreground("remove fork file")?;
    remove_unlocked(concern, skill_id)
}

/// [`remove`] for callers that already hold the repo lock.
pub fn remove_unlocked(concern: ForkConcern, skill_id: &str) -> Result<()> {
    remove_if_present(&path_for(concern, skill_id)?)
}

/// Remove every known concern's file for a skill (on delete).
pub fn remove_all_for(skill_id: &str) -> Result<()> {
    let _lock = RepoLock::acquire_foreground("remove fork files")?;
    remove_all_for_unlocked(skill_id)
}

/// [`remove_all_for`] for callers that already hold the repo lock.
pub fn remove_all_for_unlocked(skill_id: &str) -> Result<()> {
    for concern in ForkConcern::ALL {
        remove_unlocked(concern, skill_id)?;
    }
    Ok(())
}

/// Remove fork files whose skill id is no longer in the store. Files of
/// unknown concerns, temp leftovers and anything else are left alone.
pub fn sweep_orphans(store: &SkillStore) -> Result<()> {
    let _lock = RepoLock::acquire_foreground("sweep fork files")?;
    sweep_orphans_unlocked(store)
}

/// [`sweep_orphans`] for callers that already hold the repo lock.
pub fn sweep_orphans_unlocked(store: &SkillStore) -> Result<()> {
    let dir = sync_metadata::metadata_dir();
    if !dir.is_dir() {
        return Ok(());
    }
    let live: HashSet<String> = store.get_all_skills()?.into_iter().map(|s| s.id).collect();
    for entry in fs::read_dir(&dir)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        let Some((concern, skill_id)) = parse_file_name(&name) else {
            continue;
        };
        if ForkConcern::from_stem(concern).is_some() && !live.contains(skill_id) {
            remove_if_present(&entry.path())?;
        }
    }
    Ok(())
}

/// Split `fork-<concern>.<skill_id>.json` into (`fork-<concern>`, `skill_id`).
/// Temp leftovers (`….json.tmp.<uuid>`) don't end in `.json` and never match.
fn parse_file_name(name: &str) -> Option<(&str, &str)> {
    if !name.starts_with(FILE_PREFIX) {
        return None;
    }
    let body = name.strip_suffix(FILE_SUFFIX)?;
    let (stem, skill_id) = body.split_once('.')?;
    validate_skill_id(skill_id).ok()?;
    Some((stem, skill_id))
}

/// Skill ids become part of a file name: refuse anything that could escape
/// the metadata root or be mistaken for another file's name.
fn validate_skill_id(skill_id: &str) -> Result<()> {
    if skill_id.is_empty()
        || !skill_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        bail!("invalid skill id for a fork file: {skill_id:?}");
    }
    Ok(())
}

fn remove_if_present(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err.into()),
    }
}

// Copied from sync_metadata (private there, an upstream file): same temp name,
// fsync, rename, then parent-directory sync, and the same canonical encoding.
fn atomic_write_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let bytes = sync_metadata::canonical_json_bytes(value)?;
    let tmp = path.with_extension(format!("json.tmp.{}", uuid::Uuid::now_v7()));
    {
        let mut file = fs::File::create(&tmp)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
    }
    fs::rename(&tmp, path)?;
    sync_parent_dir(path)?;
    Ok(())
}

fn sync_parent_dir(path: &Path) -> Result<()> {
    let Some(parent) = path.parent() else {
        return Ok(());
    };

    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;

        // A directory handle needs FILE_FLAG_BACKUP_SEMANTICS, and
        // FlushFileBuffers needs write access (see sync_metadata).
        const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x02000000;

        let dir = fs::OpenOptions::new()
            .write(true)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(parent)?;
        dir.sync_all()?;
        Ok(())
    }

    #[cfg(not(windows))]
    {
        let dir = fs::File::open(parent)?;
        dir.sync_all()?;
        Ok(())
    }
}

#[cfg(test)]
mod storage_contract_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::central_repo;
    use crate::core::skill_store::SkillRecord;
    use serde::Deserialize;
    use std::sync::MutexGuard;
    use tempfile::{TempDir, tempdir};

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct Sample {
        value: String,
        n: u32,
    }

    struct TestRepo {
        _lock: MutexGuard<'static, ()>,
        _tmp: TempDir,
        store: SkillStore,
    }

    impl Drop for TestRepo {
        fn drop(&mut self) {
            central_repo::set_test_base_dir_override(None);
        }
    }

    fn test_repo() -> TestRepo {
        let lock = central_repo::test_base_dir_lock();
        let tmp = tempdir().unwrap();
        let base = tmp.path().join("repo");
        central_repo::set_test_base_dir_override(Some(base.clone()));
        fs::create_dir_all(central_repo::skills_dir()).unwrap();
        let store = SkillStore::new(&base.join("test.db")).unwrap();
        TestRepo {
            _lock: lock,
            _tmp: tmp,
            store,
        }
    }

    fn insert_skill(store: &SkillStore, id: &str) {
        let dir = central_repo::skills_dir().join(id);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("SKILL.md"), "---\nname: x\n---\n").unwrap();
        let now = chrono::Utc::now().timestamp_millis();
        store
            .insert_skill(&SkillRecord {
                id: id.to_string(),
                name: id.to_string(),
                description: None,
                source_type: "local".to_string(),
                source_ref: None,
                source_ref_resolved: None,
                source_subpath: None,
                source_branch: None,
                source_revision: None,
                remote_revision: None,
                central_path: dir.to_string_lossy().to_string(),
                content_hash: None,
                enabled: true,
                created_at: now,
                updated_at: now,
                status: "ok".to_string(),
                update_status: "unknown".to_string(),
                last_checked_at: None,
                last_check_error: None,
            })
            .unwrap();
    }

    fn sample(value: &str) -> Sample {
        Sample {
            value: value.to_string(),
            n: 7,
        }
    }

    #[test]
    fn round_trip_per_concern_and_missing_reads_none() {
        let _repo = test_repo();
        for concern in ForkConcern::ALL {
            assert_eq!(read::<Sample>(concern, "skill-1").unwrap(), None);
            write(concern, "skill-1", &sample(concern.stem())).unwrap();
            let path = path_for(concern, "skill-1").unwrap();
            assert_eq!(path.parent().unwrap(), sync_metadata::metadata_dir());
            assert_eq!(
                path.file_name().unwrap().to_string_lossy(),
                format!("{}.skill-1.json", concern.stem())
            );
            assert_eq!(
                read::<Sample>(concern, "skill-1").unwrap(),
                Some(sample(concern.stem()))
            );
            // Canonical encoding, same as sync_metadata's files.
            assert_eq!(
                fs::read(&path).unwrap(),
                sync_metadata::canonical_json_bytes(&sample(concern.stem())).unwrap()
            );
        }
        // Concerns don't share a file.
        remove(ForkConcern::Pin, "skill-1").unwrap();
        assert_eq!(read::<Sample>(ForkConcern::Pin, "skill-1").unwrap(), None);
        assert!(
            read::<Sample>(ForkConcern::Notes, "skill-1")
                .unwrap()
                .is_some()
        );
        // Removing what's missing is fine.
        remove(ForkConcern::Pin, "skill-1").unwrap();
    }

    #[test]
    fn rejects_skill_ids_that_could_escape_the_metadata_root() {
        let _repo = test_repo();
        for bad in ["", "..", "a/b", "a\\b", "a.b", "../x"] {
            assert!(path_for(ForkConcern::Pin, bad).is_err(), "{bad:?}");
            assert!(
                write(ForkConcern::Pin, bad, &sample("x")).is_err(),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn crashed_write_leftover_does_not_break_read_and_is_cleaned_up() {
        let _repo = test_repo();
        write(ForkConcern::Baseline, "skill-1", &sample("good")).unwrap();
        let leftover =
            sync_metadata::metadata_dir().join("fork-baseline.skill-1.json.tmp.0000-dead-beef");
        fs::write(&leftover, "{ half-writ").unwrap();

        assert_eq!(
            read::<Sample>(ForkConcern::Baseline, "skill-1").unwrap(),
            Some(sample("good"))
        );
        // The orphan sweep doesn't mistake it for a fork file.
        assert_eq!(
            parse_file_name("fork-baseline.skill-1.json.tmp.0000-dead-beef"),
            None
        );

        sync_metadata::cleanup_temporary_files().unwrap();
        assert!(!leftover.exists());
        assert_eq!(
            read::<Sample>(ForkConcern::Baseline, "skill-1").unwrap(),
            Some(sample("good"))
        );
    }

    #[test]
    fn remove_all_for_removes_every_concern_of_that_skill_only() {
        let _repo = test_repo();
        for concern in ForkConcern::ALL {
            write(concern, "gone", &sample("g")).unwrap();
            write(concern, "kept", &sample("k")).unwrap();
        }
        remove_all_for("gone").unwrap();
        for concern in ForkConcern::ALL {
            assert!(!path_for(concern, "gone").unwrap().exists());
            assert!(path_for(concern, "kept").unwrap().exists());
        }
    }

    #[test]
    fn deleting_a_skill_through_the_command_leaves_no_fork_files() {
        let repo = test_repo();
        insert_skill(&repo.store, "doomed");
        insert_skill(&repo.store, "survivor");
        for concern in ForkConcern::ALL {
            write(concern, "doomed", &sample("d")).unwrap();
            write(concern, "survivor", &sample("s")).unwrap();
        }
        crate::commands::skills::delete_managed_skills_by_ids(&repo.store, &["doomed".to_string()])
            .unwrap();
        let leftovers: Vec<_> = fs::read_dir(sync_metadata::metadata_dir())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.starts_with("fork-") && n.contains(".doomed."))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
        for concern in ForkConcern::ALL {
            assert!(path_for(concern, "survivor").unwrap().exists());
        }
    }

    #[test]
    fn sweep_orphans_removes_gone_ids_and_keeps_the_rest() {
        let repo = test_repo();
        insert_skill(&repo.store, "alive");
        for concern in ForkConcern::ALL {
            write(concern, "alive", &sample("a")).unwrap();
            write(concern, "dead", &sample("d")).unwrap();
        }
        let dir = sync_metadata::metadata_dir();
        // A newer build's concern, for a gone id: not ours to delete.
        fs::write(dir.join("fork-future.dead.json"), "{}").unwrap();
        // Unrelated files in the metadata root.
        fs::write(dir.join("schema.json"), "{}").unwrap();
        fs::write(dir.join("fork-pin.dead.json.tmp.0000-dead-beef"), "x").unwrap();

        sweep_orphans(&repo.store).unwrap();

        for concern in ForkConcern::ALL {
            assert!(path_for(concern, "alive").unwrap().exists());
            assert!(!path_for(concern, "dead").unwrap().exists());
        }
        assert!(dir.join("fork-future.dead.json").exists());
        assert!(dir.join("schema.json").exists());
        assert!(dir.join("fork-pin.dead.json.tmp.0000-dead-beef").exists());
    }

    #[test]
    fn write_all_from_db_sweeps_orphans() {
        let repo = test_repo();
        insert_skill(&repo.store, "alive");
        write(ForkConcern::Watch, "alive", &sample("a")).unwrap();
        write(ForkConcern::Watch, "dead", &sample("d")).unwrap();
        sync_metadata::write_all_from_db(&repo.store).unwrap();
        assert!(path_for(ForkConcern::Watch, "alive").unwrap().exists());
        assert!(!path_for(ForkConcern::Watch, "dead").unwrap().exists());
    }
}
