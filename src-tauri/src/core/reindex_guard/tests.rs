//! sm-69q.22 and the failed-reindex half of sm-69q.18, on scratch libraries:
//! the reindex and the metadata rewrite as the app runs them
//! (`reindex_from_metadata_unlocked`, then `write_all_from_db_unlocked`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::core::central_repo;
use crate::core::skill_store::{ScenarioRecord, SkillRecord, SkillStore};
use crate::core::sync_metadata::{
    self, ScenarioMetaFile, SkillMetaFile, SourceMeta,
};

struct Lib {
    _guard: std::sync::MutexGuard<'static, ()>,
    tmp: tempfile::TempDir,
    store: SkillStore,
}

impl Drop for Lib {
    fn drop(&mut self) {
        central_repo::set_test_base_dir_override(None);
    }
}

fn lib() -> Lib {
    let guard = central_repo::test_base_dir_lock();
    let tmp = tempfile::tempdir().unwrap();
    let base = tmp.path().join("base");
    central_repo::set_test_base_dir_override(Some(base.clone()));
    std::fs::create_dir_all(central_repo::skills_dir()).unwrap();
    let store = SkillStore::new(&base.join("db.sqlite")).unwrap();
    Lib { _guard: guard, tmp, store }
}

fn meta_dir() -> PathBuf {
    sync_metadata::metadata_dir()
}

fn record_file(id: &str) -> PathBuf {
    meta_dir().join("skills").join(format!("{id}.json"))
}

fn skill_dir(rel: &str) -> PathBuf {
    let dir = central_repo::skills_dir().join(rel);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("SKILL.md"), format!("---\nname: {rel}\n---\nbody\n")).unwrap();
    dir
}

fn record(id: &str, dir: &Path) -> SkillRecord {
    SkillRecord {
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
        update_status: "local_only".to_string(),
        last_checked_at: None,
        last_check_error: None,
    }
}

/// A skill recorded the app's way: folder, row, metadata.
fn add_skill(store: &SkillStore, id: &str) -> PathBuf {
    let dir = skill_dir(id);
    store.insert_skill(&record(id, &dir)).unwrap();
    sync_metadata::write_all_from_db_unlocked(store).unwrap();
    dir
}

/// A record file as another device (or a pull) leaves it.
fn write_record_file(id: &str, path: &str) {
    let meta = SkillMetaFile {
        schema_version: 1,
        skill_id: id.to_string(),
        path: path.to_string(),
        path_key: sync_metadata::path_key(path),
        enabled: true,
        tags: vec!["pulled".to_string()],
        source: SourceMeta {
            source_type: "local".to_string(),
            ref_: None,
            subpath: None,
            branch: None,
        },
    };
    std::fs::create_dir_all(meta_dir().join("skills")).unwrap();
    std::fs::write(record_file(id), sync_metadata::canonical_json_bytes(&meta).unwrap()).unwrap();
}

fn write_preset_file(id: &str, name: &str) {
    let meta = ScenarioMetaFile {
        schema_version: 1,
        scenario_id: id.to_string(),
        name: name.to_string(),
        description: None,
        icon: None,
        sort_order: 0,
    };
    std::fs::write(
        meta_dir().join("scenarios").join(format!("{id}.json")),
        sync_metadata::canonical_json_bytes(&meta).unwrap(),
    )
    .unwrap();
}

/// Everything the reindex writes, so "as it was" can be compared.
fn db_state(store: &SkillStore) -> String {
    let skills: BTreeMap<String, String> = store
        .get_all_skills()
        .unwrap()
        .into_iter()
        .map(|s| (s.id, s.central_path))
        .collect();
    let tags: BTreeMap<String, Vec<String>> = store.get_tags_map().unwrap().into_iter().collect();
    let presets: BTreeMap<String, (String, Vec<String>)> = store
        .get_all_scenarios()
        .unwrap()
        .into_iter()
        .map(|p| {
            let members = store.get_skill_ids_for_scenario(&p.id).unwrap();
            (p.id, (p.name, members))
        })
        .collect();
    format!("skills={skills:?}\ntags={tags:?}\npresets={presets:?}")
}

/// Library before: skills `kept` and `leaving`, preset Work = {kept}. Then
/// the disk changes as a pull from another device would change it: `arrived`
/// is added, `leaving` is deleted, and two presets with the same name come in
/// (two devices each made a "Review" preset). `scenarios.name` is UNIQUE, so
/// the reindex fails after it has already deleted and upserted skill rows.
fn library_whose_reindex_fails(store: &SkillStore) {
    add_skill(store, "kept");
    add_skill(store, "leaving");
    store
        .insert_scenario(&ScenarioRecord {
            id: "work".to_string(),
            name: "Work".to_string(),
            description: None,
            icon: None,
            sort_order: 0,
            created_at: 1,
            updated_at: 1,
        })
        .unwrap();
    store.add_skill_to_scenario("work", "kept").unwrap();
    sync_metadata::write_all_from_db_unlocked(store).unwrap();

    skill_dir("arrived");
    write_record_file("arrived", "arrived");
    std::fs::remove_dir_all(central_repo::skills_dir().join("leaving")).unwrap();
    std::fs::remove_file(record_file("leaving")).unwrap();
    write_preset_file("review-a", "Review");
    write_preset_file("review-b", "Review");
}

#[test]
fn a_failed_reindex_leaves_the_database_as_it_was() {
    let lib = lib();
    library_whose_reindex_fails(&lib.store);
    let before = db_state(&lib.store);

    let err = sync_metadata::reindex_from_metadata_unlocked(&lib.store).unwrap_err();
    assert!(format!("{err:#}").contains("UNIQUE"), "{err:#}");

    assert_eq!(db_state(&lib.store), before, "the failed reindex changed the database");
}

/// sm-69q.18 through sm-69q.22: whatever the database holds after a failed
/// reindex, it is not what is on disk, and the next metadata rewrite (the
/// auto-backup's first step) must not delete the records it does not know.
#[test]
fn a_write_after_a_failed_reindex_keeps_the_records_on_disk() {
    let lib = lib();
    library_whose_reindex_fails(&lib.store);
    sync_metadata::reindex_from_metadata_unlocked(&lib.store).unwrap_err();

    sync_metadata::write_all_from_db_unlocked(&lib.store).unwrap();
    assert!(
        record_file("arrived").is_file(),
        "the record of a skill on disk was deleted after a failed reindex"
    );
    assert!(meta_dir().join("scenarios/review-a.json").is_file());
    assert!(meta_dir().join("scenarios/review-b.json").is_file());

    // The duplicate is sorted out; the next reindex succeeds and the rewrite
    // goes back to removing what the database no longer has.
    std::fs::remove_file(meta_dir().join("scenarios/review-b.json")).unwrap();
    write_record_file("ghost", "ghost"); // a record whose folder is gone
    sync_metadata::reindex_from_metadata_unlocked(&lib.store).unwrap();
    assert!(lib.store.get_skill_by_id("arrived").unwrap().is_some());
    assert!(lib.store.get_skill_by_id("leaving").unwrap().is_none());
    assert!(lib.store.get_skill_by_id("ghost").unwrap().is_none());
    sync_metadata::write_all_from_db_unlocked(&lib.store).unwrap();
    assert!(record_file("arrived").is_file());
    assert!(!record_file("ghost").exists(), "deletions did not resume after a good reindex");
}

/// Point `link` at `target`: a symlink, or on Windows without the privilege
/// for one, a junction. Returns false when neither can be made here.
fn link_dir(target: &Path, link: &Path) -> bool {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, link).is_ok()
    }
    #[cfg(windows)]
    {
        if std::os::windows::fs::symlink_dir(target, link).is_ok() {
            return true;
        }
        std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(link)
            .arg(target)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }
}

/// A skill whose folder is a link to a drive that is not mounted at startup.
#[test]
fn an_unmounted_linked_skill_keeps_its_record() {
    let lib = lib();
    add_skill(&lib.store, "plain");
    let target = lib.tmp.path().join("external").join("linked");
    std::fs::create_dir_all(&target).unwrap();
    std::fs::write(target.join("SKILL.md"), "---\nname: linked\n---\nbody\n").unwrap();
    let link = central_repo::skills_dir().join("linked");
    if !link_dir(&target, &link) {
        eprintln!("skipped: cannot create a directory link here");
        return;
    }
    lib.store.insert_skill(&record("linked", &link)).unwrap();
    lib.store.set_tags_for_skill("linked", &["mine".to_string()]).unwrap();
    sync_metadata::write_all_from_db_unlocked(&lib.store).unwrap();
    sync_metadata::reindex_from_metadata_unlocked(&lib.store).unwrap();
    let hash = lib.store.get_skill_by_id("linked").unwrap().unwrap().content_hash;
    assert!(hash.is_some());

    // The drive goes away.
    let unmounted = lib.tmp.path().join("external-unmounted");
    std::fs::rename(lib.tmp.path().join("external"), &unmounted).unwrap();
    assert!(!link.is_dir());

    sync_metadata::reindex_from_metadata_unlocked(&lib.store).unwrap();
    let kept = lib.store.get_skill_by_id("linked").unwrap();
    assert!(kept.is_some(), "the record of an unmounted linked skill was dropped");
    let kept = kept.unwrap();
    assert_eq!(kept.central_path, link.to_string_lossy());
    assert_eq!(kept.content_hash, hash);
    assert_eq!(lib.store.get_tags_map().unwrap().remove("linked"), Some(vec!["mine".to_string()]));
    sync_metadata::write_all_from_db_unlocked(&lib.store).unwrap();
    assert!(record_file("linked").is_file());

    // The drive is back: nothing to recover.
    std::fs::rename(&unmounted, lib.tmp.path().join("external")).unwrap();
    sync_metadata::reindex_from_metadata_unlocked(&lib.store).unwrap();
    assert!(lib.store.get_skill_by_id("linked").unwrap().is_some());

    // A folder that is really gone still drops its record.
    std::fs::remove_dir_all(central_repo::skills_dir().join("plain")).unwrap();
    sync_metadata::reindex_from_metadata_unlocked(&lib.store).unwrap();
    assert!(lib.store.get_skill_by_id("plain").unwrap().is_none());
}

/// Make `dir` case-sensitive where the platform can; false when it cannot.
fn make_case_sensitive(dir: &Path) -> bool {
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("fsutil")
            .args(["file", "setCaseSensitiveInfo"])
            .arg(dir)
            .arg("enable")
            .output();
    }
    let probe = dir.join("case-probe");
    std::fs::create_dir_all(&probe).unwrap();
    let sensitive = !dir.join("CASE-PROBE").exists();
    std::fs::remove_dir_all(&probe).unwrap();
    sensitive
}

/// The record says `My-Skill`, the folder in the tree is `my-skill`: a
/// case-only rename made on a case-insensitive device that Git did not see,
/// checked out on a case-sensitive one.
#[test]
fn a_case_only_mismatch_keeps_its_record() {
    let lib = lib();
    if !make_case_sensitive(&central_repo::skills_dir()) {
        eprintln!("skipped: no case-sensitive directory available here");
        return;
    }
    add_skill(&lib.store, "other");
    let actual = skill_dir("my-skill");
    write_record_file("cased", "My-Skill");

    sync_metadata::reindex_from_metadata_unlocked(&lib.store).unwrap();
    let kept = lib.store.get_skill_by_id("cased").unwrap();
    assert!(kept.is_some(), "the record of a case-mismatched folder was dropped");
    assert_eq!(kept.unwrap().central_path, actual.to_string_lossy());

    sync_metadata::write_all_from_db_unlocked(&lib.store).unwrap();
    let written: SkillMetaFile =
        serde_json::from_slice(&std::fs::read(record_file("cased")).unwrap()).unwrap();
    assert_eq!(written.path, "my-skill");
}

// ── the write guard must not stop a deliberate delete from spreading ──

fn git(dir: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.email=test@example.com", "-c", "user.name=Test"])
        .args(args)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

struct Device {
    base: PathBuf,
    store: SkillStore,
}

impl Device {
    fn new(root: &Path, name: &str) -> Self {
        let base = root.join(name);
        std::fs::create_dir_all(&base).unwrap();
        let store = SkillStore::new(&base.join("db.sqlite")).unwrap();
        let device = Self { base, store };
        device.activate();
        std::fs::create_dir_all(central_repo::skills_dir()).unwrap();
        device
    }

    fn activate(&self) {
        central_repo::set_test_base_dir_override(Some(self.base.clone()));
    }

    fn skills(&self) -> PathBuf {
        self.activate();
        central_repo::skills_dir()
    }

    /// Back up and push, as the app's sync does it.
    fn sync(&self) {
        let skills = self.skills();
        sync_metadata::write_all_from_db_unlocked(&self.store).unwrap();
        if crate::core::git_backup::has_uncommitted_changes(&skills).unwrap() {
            crate::core::git_backup::commit_all(&skills, "backup").unwrap();
        }
        if crate::core::git_backup::get_status(&skills).unwrap().behind > 0 {
            crate::core::merge::gated_pull_unlocked(&self.store, &skills).unwrap();
            crate::commands::git_backup::reconcile_skills_index_unlocked(&self.store).unwrap();
        }
        git(&skills, &["push", "-q", "origin", "HEAD:main"]);
    }

    /// Fetch and pull, as the app's sync does it.
    fn pull(&self) {
        let skills = self.skills();
        crate::core::git_backup::fetch_remote(&skills).unwrap();
        crate::core::merge::gated_pull_unlocked(&self.store, &skills).unwrap();
        crate::commands::git_backup::reconcile_skills_index_unlocked(&self.store).unwrap();
        sync_metadata::write_all_from_db_unlocked(&self.store).unwrap();
    }

    fn has(&self, id: &str) -> (bool, bool, bool) {
        let skills = self.skills();
        (
            self.store.get_skill_by_id(id).unwrap().is_some(),
            record_file(id).exists(),
            skills.join(id).exists(),
        )
    }
}

/// Device A deletes a skill and syncs; device B syncs and the skill is gone
/// there: no row, no record file, no folder. Run twice: once as usual, once
/// with A's deletions deferred by a failed reindex, where A's sync carries
/// the folder's removal but not yet the record file's.
#[test]
fn a_skill_deleted_on_one_device_is_gone_on_the_other_after_sync() {
    for a_reindex_failed in [false, true] {
        let _guard = central_repo::test_base_dir_lock();
        let tmp = tempfile::tempdir().unwrap();
        let remote = tmp.path().join("remote.git");
        git(tmp.path(), &["init", "-q", "--bare", "-b", "main", remote.to_str().unwrap()]);

        let a = Device::new(tmp.path(), "a");
        crate::core::git_backup::init_repo(&a.skills(), "Device A").unwrap();
        a.activate();
        add_skill(&a.store, "doomed");
        add_skill(&a.store, "stays");
        git(&a.skills(), &["remote", "add", "origin", remote.to_str().unwrap()]);
        a.sync();
        git(&a.skills(), &["branch", "-q", "--set-upstream-to=origin/main"]);

        let b = Device::new(tmp.path(), "b");
        crate::core::git_backup::clone_into_strict(&b.skills(), remote.to_str().unwrap()).unwrap();
        crate::core::git_backup::configure_device_identity(&b.skills(), "Device B").unwrap();
        b.activate();
        sync_metadata::reindex_from_metadata_unlocked(&b.store).unwrap();
        assert_eq!(b.has("doomed"), (true, true, true));

        // A deletes the skill the way the app does: folder, row, metadata.
        a.activate();
        if a_reindex_failed {
            super::record_reindex_outcome(&Err(anyhow::anyhow!("an earlier reindex failed")));
        }
        std::fs::remove_dir_all(a.skills().join("doomed")).unwrap();
        a.store.delete_skill("doomed").unwrap();
        a.sync();
        assert_eq!(a.has("doomed").2, false);
        assert_eq!(a.has("doomed").1, a_reindex_failed, "record file on A");

        b.pull();
        assert_eq!(
            b.has("doomed"),
            (false, false, false),
            "deleted skill still on device B (A's reindex failed: {a_reindex_failed})"
        );
        assert_eq!(b.has("stays"), (true, true, true));

        // B's sync carries the record file's removal back to A, whose next
        // good reindex lifts the deferral.
        b.sync();
        a.pull();
        assert_eq!(a.has("doomed"), (false, false, false));
        assert!(super::deletions_allowed());
        central_repo::set_test_base_dir_override(None);
    }
}
