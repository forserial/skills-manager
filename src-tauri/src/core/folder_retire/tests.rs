use super::test_hooks::with_failing_rename;
use super::*;
use crate::commands::skills::delete_managed_skills_by_ids;
use crate::core::central_repo;
use crate::core::error::ErrorKind;
use crate::core::skill_store::SkillRecord;
use std::fs;
use std::sync::MutexGuard;
use tempfile::{tempdir, TempDir};

struct Library {
    _lock: MutexGuard<'static, ()>,
    _tmp: TempDir,
    root: PathBuf,
    store: SkillStore,
}

impl Drop for Library {
    fn drop(&mut self) {
        central_repo::set_test_base_dir_override(None);
    }
}

fn library() -> Library {
    let lock = central_repo::test_base_dir_lock();
    let tmp = tempdir().unwrap();
    let base = tmp.path().join("base");
    central_repo::set_test_base_dir_override(Some(base.clone()));
    let root = central_repo::skills_dir();
    fs::create_dir_all(&root).unwrap();
    let store = SkillStore::new(&base.join("test.db")).unwrap();
    Library {
        _lock: lock,
        _tmp: tmp,
        root,
        store,
    }
}

impl Library {
    /// A managed skill with a nested file, so a half-removed folder would show.
    fn skill(&self, id: &str) -> PathBuf {
        let dir = self.root.join(id);
        fs::create_dir_all(dir.join("scripts")).unwrap();
        fs::write(dir.join("SKILL.md"), format!("---\nname: {id}\n---\n")).unwrap();
        fs::write(dir.join("scripts/run.sh"), b"echo hi\n").unwrap();
        self.store
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
                created_at: 1,
                updated_at: 1,
                status: "ok".to_string(),
                update_status: "up_to_date".to_string(),
                last_checked_at: None,
                last_check_error: None,
            })
            .unwrap();
        dir
    }

    /// Names directly under the library root, sorted, minus the metadata dir.
    fn entries(&self) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(&self.root)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n != ".skills-manager")
            .collect();
        names.sort();
        names
    }
}

#[test]
fn retire_then_discard_removes_the_folder_and_leaves_nothing_behind() {
    let lib = library();
    let dir = lib.skill("one");

    let retired = retire(&dir).unwrap();
    assert!(!dir.exists(), "the skill's own name is free at once");
    assert_eq!(lib.entries().len(), 1, "moved aside, not yet removed");
    assert!(lib.entries()[0].starts_with(".one.deleting-"));

    retired.discard();
    assert!(lib.entries().is_empty(), "{:?}", lib.entries());
}

#[test]
fn retiring_a_missing_folder_is_not_a_failure() {
    let lib = library();
    let retired = retire(&lib.root.join("never-existed")).unwrap();
    retired.discard();
    assert!(lib.entries().is_empty());
}

#[test]
fn a_failed_rename_changes_nothing() {
    let lib = library();
    let dir = lib.skill("one");

    let err = with_failing_rename(|| retire(&dir))
        .err()
        .expect("rename fails");

    assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
    assert!(dir.join("SKILL.md").is_file());
    assert!(dir.join("scripts/run.sh").is_file());
    assert_eq!(lib.entries(), vec!["one".to_string()]);
}

#[test]
fn restore_puts_the_folder_back_whole() {
    let lib = library();
    let dir = lib.skill("one");

    retire(&dir).unwrap().restore();

    assert!(dir.join("SKILL.md").is_file());
    assert!(dir.join("scripts/run.sh").is_file());
    assert_eq!(lib.entries(), vec!["one".to_string()]);
}

#[test]
fn a_normal_delete_still_works_and_leaves_no_trace_in_the_library() {
    let lib = library();
    lib.skill("doomed");
    let keeper = lib.skill("keeper");

    let result = delete_managed_skills_by_ids(&lib.store, &["doomed".to_string()]).unwrap();

    assert_eq!(result.deleted, 1);
    assert!(result.failed.is_empty());
    assert!(result.failure_reasons.is_empty());
    assert!(lib.store.get_skill_by_id("doomed").unwrap().is_none());
    assert!(lib.store.get_skill_by_id("keeper").unwrap().is_some());
    assert!(keeper.join("SKILL.md").is_file());
    assert_eq!(lib.entries(), vec!["keeper".to_string()]);
}

#[test]
fn a_delete_whose_folder_cannot_be_moved_keeps_the_skill_and_reports_it() {
    let lib = library();
    let locked = lib.skill("locked");
    let free = lib.skill("free");

    let result = with_failing_rename(|| {
        delete_managed_skills_by_ids(&lib.store, &["locked".to_string(), "free".to_string()])
    })
    .unwrap();

    // Both renames fail here, so both skills are kept and both are reported.
    assert_eq!(result.deleted, 0);
    assert_eq!(
        result.failed,
        vec!["locked".to_string(), "free".to_string()]
    );
    assert_eq!(result.failure_reasons.len(), 2);
    assert!(result.failure_reasons[0].reason.contains("kept"));
    assert!(lib.store.get_skill_by_id("locked").unwrap().is_some());
    assert!(lib.store.get_skill_by_id("free").unwrap().is_some());
    // Whole folders, nothing moved or half removed.
    for dir in [&locked, &free] {
        assert!(dir.join("SKILL.md").is_file());
        assert!(dir.join("scripts/run.sh").is_file());
    }
    assert_eq!(
        lib.entries(),
        vec!["free".to_string(), "locked".to_string()]
    );

    let audit = lib.store.list_audit(Some(10)).unwrap();
    assert!(audit
        .iter()
        .any(|a| a.action == "remove" && !a.success && a.skill_id.as_deref() == Some("locked")));

    // The same skills delete once whatever held them lets go.
    let again = delete_managed_skills_by_ids(&lib.store, &["locked".to_string()]).unwrap();
    assert_eq!(again.deleted, 1);
    assert_eq!(lib.entries(), vec!["free".to_string()]);
}

#[test]
fn a_single_delete_that_kept_the_skill_does_not_say_not_found() {
    let lib = library();
    lib.skill("locked");

    let result =
        with_failing_rename(|| delete_managed_skills_by_ids(&lib.store, &["locked".to_string()]))
            .unwrap();
    let err = single_delete_error("locked", &result.failure_reasons);
    assert_eq!(err.kind, ErrorKind::Io);
    assert!(
        err.message.contains("folder could not be removed"),
        "{}",
        err.message
    );

    let missing = single_delete_error("nope", &result.failure_reasons);
    assert_eq!(missing.kind, ErrorKind::NotFound);
}
