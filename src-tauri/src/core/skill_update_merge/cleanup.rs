//! Clearing pending-merge records that no longer have a skill.
//!
//! A held merge keeps a device-local record at
//! `base_dir()/pending-merges/<skill_id>/` ([`super::pending`]). Nothing reads
//! it without its skill, and a new skill gets a new id, so a left-over record
//! does no harm; it is only clutter. Two best-effort entry points remove it:
//!
//! - [`forget`], when a skill is deleted;
//! - [`sweep`], beside `baseline::sweep_base_store`, for records whose skill
//!   was removed some other way (a library sync, a restore, an older build).
//!
//! Neither returns an error: failing to tidy up must never fail a delete.

use std::collections::HashSet;

use super::pending;
use crate::core::skill_store::SkillStore;

/// Remove the pending-merge record of a deleted skill. Best-effort.
pub fn forget(skill_id: &str) {
    if let Err(err) = pending::remove(skill_id) {
        log::warn!("could not remove the pending merge of {skill_id}: {err:#}");
    }
}

/// Remove the record directories whose skill no longer exists. Best-effort.
///
/// Only directory names that are valid skill ids are considered, and the
/// store must be readable: when the list of live skills is unknown nothing
/// is removed, so a database error can never cost a live skill's record.
pub fn sweep(store: &SkillStore) {
    let Ok(live) = store.get_all_skills() else {
        return;
    };
    let live: HashSet<String> = live.into_iter().map(|s| s.id).collect();
    let Ok(entries) = std::fs::read_dir(pending::root()) else {
        return;
    };
    for entry in entries.flatten() {
        let Some(id) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        if !live.contains(&id) {
            forget(&id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::skills::delete_managed_skills_by_ids;
    use crate::core::central_repo;
    use crate::core::skill_store::SkillRecord;
    use crate::core::skill_update_merge::{pending, ConflictKind, MergeConflict};
    use std::fs;
    use std::sync::MutexGuard;
    use tempfile::{tempdir, TempDir};

    struct Repo {
        _lock: MutexGuard<'static, ()>,
        _tmp: TempDir,
        store: SkillStore,
    }

    impl Drop for Repo {
        fn drop(&mut self) {
            central_repo::set_test_base_dir_override(None);
        }
    }

    fn repo() -> Repo {
        let lock = central_repo::test_base_dir_lock();
        let tmp = tempdir().unwrap();
        let base = tmp.path().join("repo");
        central_repo::set_test_base_dir_override(Some(base.clone()));
        fs::create_dir_all(central_repo::skills_dir()).unwrap();
        let store = SkillStore::new(&base.join("test.db")).unwrap();
        Repo {
            _lock: lock,
            _tmp: tmp,
            store,
        }
    }

    fn skill(id: &str) -> SkillRecord {
        let dir = central_repo::skills_dir().join(id);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("SKILL.md"), format!("---\nname: {id}\n---\n")).unwrap();
        SkillRecord {
            id: id.to_string(),
            name: id.to_string(),
            description: None,
            source_type: "git".to_string(),
            source_ref: None,
            source_ref_resolved: None,
            source_subpath: None,
            source_branch: None,
            source_revision: None,
            remote_revision: None,
            central_path: dir.to_string_lossy().to_string(),
            content_hash: None,
            enabled: true,
            created_at: 1,
            updated_at: 1,
            status: "ok".to_string(),
            update_status: "up_to_date".to_string(),
            last_checked_at: None,
            last_check_error: None,
        }
    }

    /// Hold a merge with one conflict for `record`, as an update would.
    fn hold(record: &SkillRecord) {
        let conflict = MergeConflict {
            path: "SKILL.md".to_string(),
            kind: ConflictKind::Text,
            local_sha256: Some("a".repeat(64)),
            upstream_sha256: Some("b".repeat(64)),
        };
        pending::save_hold(record, "rev2", "rev1", &[], &[conflict], |_| {
            Ok(pending::Versions {
                base: Some(b"base\n".to_vec()),
                mine: Some(b"mine\n".to_vec()),
                theirs: None,
            })
        })
        .unwrap();
        assert!(pending::dir_for(&record.id)
            .unwrap()
            .join("merge.json")
            .is_file());
    }

    fn has_record(id: &str) -> bool {
        pending::dir_for(id).unwrap().exists()
    }

    #[test]
    fn deleting_a_skill_with_a_held_merge_removes_its_record() {
        let repo = repo();
        let held = skill("held");
        let other = skill("other");
        repo.store.insert_skill(&held).unwrap();
        repo.store.insert_skill(&other).unwrap();
        hold(&held);
        hold(&other);

        let result = delete_managed_skills_by_ids(&repo.store, &["held".to_string()]).unwrap();

        assert_eq!(result.deleted, 1);
        assert!(repo.store.get_skill_by_id("held").unwrap().is_none());
        assert!(!has_record("held"));
        assert!(has_record("other"), "another skill's record is left alone");
    }

    #[test]
    fn deleting_a_skill_without_a_held_merge_is_unaffected() {
        let repo = repo();
        repo.store.insert_skill(&skill("plain")).unwrap();

        let result = delete_managed_skills_by_ids(&repo.store, &["plain".to_string()]).unwrap();

        assert_eq!(result.deleted, 1);
        assert!(result.failed.is_empty());
    }

    #[test]
    fn sweep_removes_an_orphaned_record_and_keeps_a_live_one() {
        let repo = repo();
        let live = skill("live");
        repo.store.insert_skill(&live).unwrap();
        hold(&live);
        hold(&skill("ghost"));

        sweep(&repo.store);

        assert!(has_record("live"));
        assert!(!has_record("ghost"));
    }

    #[test]
    fn sweep_with_no_records_directory_does_nothing() {
        let repo = repo();
        sweep(&repo.store);
        assert!(!pending::root().exists());
    }

    #[test]
    fn a_failure_to_remove_does_not_panic_or_fail() {
        let _repo = repo();
        // An id that names no valid record: removal errors, forget swallows it.
        forget("../escape");
        forget("");
        // A record "directory" that is really a file cannot be removed as
        // one; that is also swallowed.
        let root = pending::root();
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("a-file"), b"x").unwrap();
        forget("a-file");
    }
}
