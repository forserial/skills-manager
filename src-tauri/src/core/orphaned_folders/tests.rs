use super::*;
use crate::core::skill_store::SkillRecord;
use crate::core::sync_metadata;
use std::fs;
use std::sync::MutexGuard;
use std::time::SystemTime;
use tempfile::{TempDir, tempdir};

/// A scratch library: the base dir override points every path at a temp dir.
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
    /// A folder at `rel` with a SKILL.md and one more file.
    fn skill_folder(&self, rel: &str, name: &str) -> PathBuf {
        let dir = self.root.join(rel);
        fs::create_dir_all(dir.join("scripts")).unwrap();
        fs::write(
            dir.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: test skill {name}\n---\n\nBody\r\n"),
        )
        .unwrap();
        fs::write(dir.join("scripts/run.sh"), b"#!/bin/sh\necho hi\n").unwrap();
        dir
    }

    /// A skill with a record and its metadata file, as an install leaves it.
    fn managed(&self, id: &str, rel: &str) -> PathBuf {
        let dir = self.skill_folder(rel, id);
        let now = chrono::Utc::now().timestamp_millis();
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
                created_at: now,
                updated_at: now,
                status: "ok".to_string(),
                update_status: "local_only".to_string(),
                last_checked_at: None,
                last_check_error: None,
            })
            .unwrap();
        sync_metadata::write_all_from_db_unlocked(&self.store).unwrap();
        dir
    }

    fn found(&self) -> Vec<(String, bool)> {
        find(&self.store)
            .unwrap()
            .into_iter()
            .map(|f| (f.dir_name, f.has_skill_md))
            .collect()
    }
}

/// Every entry under `dir`, with each file's bytes and modification time:
/// equal snapshots mean nothing was written, added or removed inside. Folder
/// times are left out: Windows reports a folder's from its parent's listing,
/// which it updates lazily, and an added or removed entry shows in the set.
fn snapshot(dir: &Path) -> Vec<(PathBuf, Option<(Vec<u8>, SystemTime)>)> {
    let mut entries: Vec<_> = walkdir::WalkDir::new(dir)
        .into_iter()
        .map(|e| e.unwrap())
        .map(|e| {
            let meta = fs::metadata(e.path()).unwrap();
            let file = meta
                .is_file()
                .then(|| (fs::read(e.path()).unwrap(), meta.modified().unwrap()));
            (e.path().strip_prefix(dir).unwrap().to_path_buf(), file)
        })
        .collect();
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    entries
}

#[test]
fn lists_folders_with_no_record_and_nothing_else() {
    let lib = library();
    lib.managed("managed", "managed");
    // A grouping folder whose only skill is managed, one level down.
    lib.managed("inner", "group/inner");
    lib.skill_folder("orphan", "orphan");
    fs::create_dir_all(lib.root.join("notes")).unwrap();
    fs::write(lib.root.join("notes/readme.txt"), "not a skill").unwrap();
    // What sits in a real library root besides skills.
    fs::create_dir_all(lib.root.join(".git/objects")).unwrap();
    lib.skill_folder(".hidden", "hidden");
    lib.skill_folder(".orphan.staged-1234", "orphan");
    lib.skill_folder("skills-local-copies-20261110-120000", "copies");
    lib.skill_folder("skills-backup-before-clone-20261110-120000", "backup");
    fs::create_dir_all(lib.root.join("__pycache__")).unwrap();
    lib.skill_folder("scratch-area", "scratch");
    lib.skill_folder("keep-me", "keep");
    fs::write(lib.root.join("loose-file.md"), "a file, not a folder").unwrap();
    fs::write(
        lib.root.join(".gitignore"),
        ".DS_Store\n__pycache__/\n*.pyc\n# mine\n/scratch-*/\nkeep-*\n!keep-me\ndeep/path/\n",
    )
    .unwrap();

    assert_eq!(
        lib.found(),
        vec![
            ("keep-me".to_string(), true),
            ("notes".to_string(), false),
            ("orphan".to_string(), true),
        ]
    );
    let orphan = find(&lib.store)
        .unwrap()
        .into_iter()
        .find(|f| f.dir_name == "orphan")
        .unwrap();
    assert_eq!(orphan.name.as_deref(), Some("orphan"));
    assert_eq!(PathBuf::from(&orphan.path), lib.root.join("orphan"));
    assert!(orphan.size > 0);
    assert!(orphan.modified.is_some());
}

#[test]
fn a_folder_a_metadata_file_names_is_not_orphaned() {
    let lib = library();
    lib.managed("from-metadata", "from-metadata");
    // The record is gone but its metadata file stays: the next reindex brings
    // the skill back, so offering the folder would make a second record.
    lib.store.delete_skill("from-metadata").unwrap();
    assert!(lib.found().is_empty());
}

#[test]
fn an_oversized_exclusion_does_not_hide_an_orphan() {
    let lib = library();
    lib.skill_folder("huge", "huge");
    crate::core::git_backup::apply_oversized_exclusions(&lib.root, 1).unwrap();
    let gitignore = fs::read_to_string(lib.root.join(".gitignore")).unwrap();
    // The section markers here are git_backup's own.
    assert!(gitignore.contains(OVERSIZED_SECTION_BEGIN), "{gitignore}");
    assert!(gitignore.contains(OVERSIZED_SECTION_END), "{gitignore}");
    assert!(gitignore.contains("/huge/"), "{gitignore}");
    assert_eq!(lib.found(), vec![("huge".to_string(), true)]);
}

#[test]
fn adopting_makes_one_record_in_place_and_leaves_the_files_alone() {
    let lib = library();
    lib.managed("managed", "managed");
    let dir = lib.skill_folder("orphan", "orphan-skill");
    let before = snapshot(&dir);
    let records_before = lib.store.get_all_skills().unwrap().len();

    let adopted = adopt(&lib.store, "orphan").unwrap();

    assert_eq!(snapshot(&dir), before, "nothing is written inside the folder");
    let skills = lib.store.get_all_skills().unwrap();
    assert_eq!(skills.len(), records_before + 1);
    let record = lib.store.get_skill_by_id(&adopted.skill_id).unwrap().unwrap();
    assert_eq!(PathBuf::from(&record.central_path), dir);
    assert_eq!(record.name, "orphan-skill");
    assert_eq!(record.source_type, "local");
    assert_eq!(record.source_ref, None);
    assert_eq!(record.update_status, "local_only");
    assert_eq!(
        record.content_hash.as_deref(),
        Some(content_hash::hash_directory(&dir).unwrap().as_str())
    );
    assert_eq!(
        skills.iter().filter(|s| PathBuf::from(&s.central_path) == dir).count(),
        1
    );
    // Its metadata file is beside the others, not in the folder.
    assert!(lib
        .root
        .join(METADATA_DIR)
        .join("skills")
        .join(format!("{}.json", adopted.skill_id))
        .is_file());
    // Nothing else in the library root appeared or moved.
    let mut root_entries: Vec<_> = fs::read_dir(&lib.root)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
        .collect();
    root_entries.sort();
    assert_eq!(root_entries, vec![".skills-manager", "managed", "orphan"]);
    assert!(lib.found().is_empty());
}

#[test]
fn a_startup_reindex_keeps_an_adopted_skill() {
    let lib = library();
    lib.skill_folder("orphan", "orphan");
    let adopted = adopt(&lib.store, "orphan").unwrap();

    sync_metadata::reindex_from_metadata(&lib.store).unwrap();

    let record = lib.store.get_skill_by_id(&adopted.skill_id).unwrap().unwrap();
    assert_eq!(PathBuf::from(&record.central_path), lib.root.join("orphan"));
    assert_eq!(record.name, "orphan");
    assert_eq!(lib.store.get_all_skills().unwrap().len(), 1);
}

#[test]
fn a_folder_without_skill_md_is_listed_but_not_adopted() {
    let lib = library();
    fs::create_dir_all(lib.root.join("notes")).unwrap();
    fs::write(lib.root.join("notes/readme.txt"), "not a skill").unwrap();
    let before = snapshot(&lib.root.join("notes"));

    assert_eq!(lib.found(), vec![("notes".to_string(), false)]);
    let err = adopt(&lib.store, "notes").unwrap_err();
    assert!(err.message.contains("has no SKILL.md"), "{}", err.message);
    assert!(err.message.contains("cannot be adopted"), "{}", err.message);
    assert!(lib.store.get_all_skills().unwrap().is_empty());
    assert_eq!(snapshot(&lib.root.join("notes")), before);
}

#[test]
fn adopt_refuses_anything_that_is_not_an_unmanaged_folder() {
    let lib = library();
    lib.managed("managed", "managed");
    lib.skill_folder(".hidden", "hidden");
    lib.skill_folder("orphan", "orphan");

    for name in ["managed", ".hidden", ".git", ".skills-manager", "missing"] {
        let err = adopt(&lib.store, name).unwrap_err();
        assert!(
            err.message.contains("is not an unmanaged folder"),
            "{name}: {}",
            err.message
        );
    }
    for name in ["../orphan", "orphan/scripts", "", ".", ".."] {
        let err = adopt(&lib.store, name).unwrap_err();
        assert!(err.message.contains("is not a folder name"), "{name}: {}", err.message);
    }
    assert_eq!(lib.store.get_all_skills().unwrap().len(), 1);
    // A trailing separator, as shell completion adds, still names the folder.
    adopt(&lib.store, "orphan/").unwrap();
}

#[test]
fn adopt_all_takes_every_skill_and_skips_the_rest() {
    let lib = library();
    lib.skill_folder("one", "one");
    lib.skill_folder("two", "two");
    fs::create_dir_all(lib.root.join("notes")).unwrap();

    let result = adopt_all(&lib.store).unwrap();

    let mut adopted: Vec<_> = result.adopted.iter().map(|a| a.dir_name.as_str()).collect();
    adopted.sort();
    assert_eq!(adopted, vec!["one", "two"]);
    assert_eq!(result.skipped.len(), 1);
    assert_eq!(result.skipped[0].dir_name, "notes");
    assert!(result.skipped[0].reason.contains("has no SKILL.md"));
    assert_eq!(lib.found(), vec![("notes".to_string(), false)]);
}

#[test]
fn ignore_rules_apply_to_root_folder_names_only() {
    let rules = IgnoreRules::parse(
        "# comment\n\n__pycache__/\n*.tmp\n/build-*\n**/cache\nsub/dir/\nlog?\n[ab]x\n!a*.tmp\n\\#hash\n\
         # skills-manager: oversized skills excluded from backup (auto-managed)\n/huge/\n\
         # skills-manager: end oversized skills\n",
    );
    for ignored in ["__pycache__", "x.tmp", "build-1", "cache", "log1", "ax", "bx", "#hash"] {
        assert!(rules.ignores_dir(ignored), "{ignored} should be ignored");
    }
    for kept in ["a.tmp", "dir", "sub", "log", "log12", "cx", "huge", "mybuild-1"] {
        assert!(!rules.ignores_dir(kept), "{kept} should not be ignored");
    }
}

#[test]
fn the_warning_names_everything_that_is_lost() {
    for lost in ["old id", "source", "baseline", "pin", "change notes", "watched upstreams"] {
        assert!(CANNOT_RECOVER.contains(lost), "{lost}");
    }
    assert!(CANNOT_RECOVER.contains("skills set-source"));
    assert!(CANNOT_RECOVER.contains("skills diff --upstream"));
}
