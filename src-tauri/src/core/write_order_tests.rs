//! A skill folder and its library record must not part company (sm-69q.19,
//! sm-69q.20). Every test runs on a scratch library: the base-dir override
//! points the library, the lock file and the metadata at a temp dir.
//!
//! "Orphaned" below means what the startup reindex would make of it: a folder
//! in the library whose metadata file is missing (the reindex drops its row),
//! or a row whose folder is missing (the reindex drops the row too, which is
//! the clean outcome for an install that never finished).

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::MutexGuard;

use tempfile::{TempDir, tempdir};

use crate::core::skill_store::{ProjectRecord, SkillRecord, SkillStore};
use crate::core::{central_repo, sync_metadata};

pub(crate) struct Library {
    _lock: MutexGuard<'static, ()>,
    pub tmp: TempDir,
    pub root: PathBuf,
    pub store: SkillStore,
}

impl Drop for Library {
    fn drop(&mut self) {
        central_repo::set_test_base_dir_override(None);
    }
}

/// A scratch library holding one managed skill, `keep`, with a complete
/// metadata snapshot, as any library that has been used once has.
pub(crate) fn library() -> Library {
    let lock = central_repo::test_base_dir_lock();
    let tmp = tempdir().unwrap();
    let base = tmp.path().join("base");
    central_repo::set_test_base_dir_override(Some(base.clone()));
    let root = central_repo::skills_dir();
    fs::create_dir_all(&root).unwrap();
    let store = SkillStore::new(&base.join("test.db")).unwrap();
    let lib = Library {
        _lock: lock,
        tmp,
        root,
        store,
    };
    let keep = lib.root.join("keep");
    write_skill(&keep, "keep");
    lib.store.insert_skill(&record("keep", &keep)).unwrap();
    sync_metadata::write_all_from_db_unlocked(&lib.store).unwrap();
    lib
}

pub(crate) fn write_skill(dir: &Path, name: &str) {
    fs::create_dir_all(dir.join("scripts")).unwrap();
    fs::write(
        dir.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: test skill {name}\n---\n\nBody\n"),
    )
    .unwrap();
    fs::write(dir.join("scripts/run.sh"), b"echo hi\n").unwrap();
}

pub(crate) fn record(id: &str, dir: &Path) -> SkillRecord {
    let now = chrono::Utc::now().timestamp_millis();
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
        created_at: now,
        updated_at: now,
        status: "ok".to_string(),
        update_status: "local_only".to_string(),
        last_checked_at: None,
        last_check_error: None,
    }
}

impl Library {
    /// The library's top-level entries, sorted, metadata and git folders left out.
    pub fn entries(&self) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(&self.root)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
            .filter(|n| n != ".skills-manager" && n != ".git")
            .collect();
        names.sort();
        names
    }

    /// Names of the skills that survive a startup reindex.
    pub fn after_reindex(&self) -> Vec<String> {
        sync_metadata::reindex_from_metadata_unlocked(&self.store).unwrap();
        let mut names: Vec<String> = self
            .store
            .get_all_skills()
            .unwrap()
            .into_iter()
            .map(|s| s.name)
            .collect();
        names.sort();
        names
    }

    /// Each record's folder exists, and each folder in the library has a record.
    pub fn assert_consistent(&self) {
        let skills = self.store.get_all_skills().unwrap();
        for skill in &skills {
            assert!(
                Path::new(&skill.central_path).is_dir(),
                "{} has a record but no folder",
                skill.name
            );
        }
        let recorded: Vec<PathBuf> = skills.iter().map(|s| PathBuf::from(&s.central_path)).collect();
        for name in self.entries() {
            assert!(
                recorded.contains(&self.root.join(&name)),
                "{name} is in the library with no record (entries: {:?})",
                self.entries()
            );
        }
    }
}

// ── sm-69q.19: project import ──

fn project_with_skill(lib: &Library, skill: &str) -> ProjectRecord {
    let project = lib.tmp.path().join("project");
    write_skill(&project.join(".claude/skills").join(skill), skill);
    let record = ProjectRecord {
        id: "p".to_string(),
        name: "project".to_string(),
        path: project.to_string_lossy().to_string(),
        workspace_type: "project".to_string(),
        linked_agent_key: None,
        linked_agent_name: None,
        disabled_path: None,
        sort_order: 0,
        created_at: 0,
        updated_at: 0,
    };
    lib.store.insert_project(&record).unwrap();
    record
}

/// The agent key and relative path the project view would pass for `skill`.
fn project_skill_args(lib: &Library, skill: &str) -> (String, String) {
    use crate::commands::projects::{agent_skill_configs, read_workspace_skills};
    let project = lib.store.get_project_by_id("p").unwrap().unwrap();
    let found = read_workspace_skills(&project, &agent_skill_configs(&lib.store));
    let info = found
        .into_iter()
        .find(|s| s.dir_name == skill)
        .expect("the project lists the skill");
    (info.agent, info.relative_path)
}

#[test]
fn a_project_import_keeps_its_record_through_the_next_reindex() {
    let lib = library();
    project_with_skill(&lib, "imported");
    let (agent, rel) = project_skill_args(&lib, "imported");

    crate::commands::projects::import_project_skill_blocking(&lib.store, "p", &rel, &agent)
        .unwrap();
    assert!(lib.root.join("imported/SKILL.md").is_file());

    // Nothing else writes metadata before the next start.
    assert_eq!(lib.after_reindex(), vec!["imported", "keep"]);
    lib.assert_consistent();
}

// ── failure injection ──
//
// Real failures where the old code can hit them too: a source file another
// process holds open makes the copy fail halfway, and a held-open
// `schema.json` makes every metadata write fail (each one rewrites it first).
// Steps that are new or cannot be failed from outside use named fail points.

/// Makes reading `path` fail until dropped.
struct HeldFile {
    #[cfg(windows)]
    _file: fs::File,
    #[cfg(unix)]
    path: PathBuf,
}

#[cfg(windows)]
fn hold(path: &Path) -> HeldFile {
    use std::os::windows::fs::OpenOptionsExt;
    let file = fs::OpenOptions::new().read(true).share_mode(0).open(path).unwrap();
    HeldFile { _file: file }
}

#[cfg(unix)]
fn hold(path: &Path) -> HeldFile {
    use std::os::unix::fs::PermissionsExt;
    // A held file cannot be replaced either: lock its folder too.
    fs::set_permissions(path, fs::Permissions::from_mode(0o000)).unwrap();
    let parent = path.parent().unwrap();
    fs::set_permissions(parent, fs::Permissions::from_mode(0o555)).unwrap();
    HeldFile { path: path.to_path_buf() }
}

#[cfg(unix)]
impl Drop for HeldFile {
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(self.path.parent().unwrap(), fs::Permissions::from_mode(0o755));
        let _ = fs::set_permissions(&self.path, fs::Permissions::from_mode(0o644));
    }
}

/// Every metadata write fails until dropped.
fn block_metadata() -> HeldFile {
    hold(&sync_metadata::metadata_dir().join("schema.json"))
}

/// A skill outside the library whose `scripts/run.sh` cannot be read.
fn unreadable_source(lib: &Library, name: &str) -> (PathBuf, HeldFile) {
    let src = lib.tmp.path().join("sources").join(name);
    write_skill(&src, name);
    let held = hold(&src.join("scripts/run.sh"));
    (src, held)
}

fn local_metadata(source: &Path) -> crate::commands::skills::InstallSourceMetadata {
    crate::commands::skills::InstallSourceMetadata {
        source_type: "local".to_string(),
        source_ref: Some(source.to_string_lossy().to_string()),
        source_ref_resolved: None,
        source_subpath: None,
        source_branch: None,
        source_revision: None,
        remote_revision: None,
        update_status: "local_only".to_string(),
    }
}

fn keep_skill_md(lib: &Library) -> String {
    fs::read_to_string(lib.root.join("keep/SKILL.md")).unwrap()
}

#[test]
fn a_copy_that_fails_halfway_leaves_no_partial_folder() {
    let lib = library();
    let (src, held) = unreadable_source(&lib, "half");
    let dest = lib.root.join("half");

    let err = crate::core::installer::install_skill_dir_to_destination(&src, "half", &dest);
    drop(held);

    assert!(err.is_err(), "the copy must fail");
    assert_eq!(lib.entries(), vec!["keep"], "nothing partial, nothing staged");
    assert_eq!(lib.after_reindex(), vec!["keep"]);
}

#[test]
fn a_copy_that_fails_keeps_the_folder_it_was_replacing() {
    let lib = library();
    let before = keep_skill_md(&lib);
    let (src, held) = unreadable_source(&lib, "keep-new");

    let err = crate::core::installer::install_skill_dir_to_destination(
        &src,
        "keep",
        &lib.root.join("keep"),
    );
    drop(held);

    assert!(err.is_err());
    assert_eq!(keep_skill_md(&lib), before, "the installed copy is untouched");
    assert!(lib.root.join("keep/scripts/run.sh").is_file());
    assert_eq!(lib.entries(), vec!["keep"]);
}

#[test]
fn a_failure_after_the_copy_or_at_the_rename_leaves_nothing() {
    for point in ["place.after_copy", "place.rename"] {
        let lib = library();
        let src = lib.tmp.path().join("sources/fresh");
        write_skill(&src, "fresh");
        let _armed = crate::core::fail_points::arm(point);

        let err = crate::core::installer::install_from_local(&src, Some("fresh"));

        assert!(err.is_err(), "{point} must fail the install");
        assert_eq!(lib.entries(), vec!["keep"], "after {point}");
    }
}

#[test]
fn an_install_that_cannot_write_metadata_takes_its_folder_back() {
    let lib = library();
    let src = lib.tmp.path().join("sources/fresh");
    write_skill(&src, "fresh");

    let result = crate::core::installer::install_from_local(&src, Some("fresh")).unwrap();
    assert!(result.created_folder);
    let blocked = block_metadata();
    let err = crate::commands::skills::store_installed_skill_unlocked(
        &lib.store,
        &result,
        &local_metadata(&src),
        None,
    );
    drop(blocked);

    assert!(err.is_err());
    assert_eq!(lib.entries(), vec!["keep"], "no folder without a record");
    assert_eq!(lib.store.get_all_skills().unwrap().len(), 1, "no row either");
    assert_eq!(lib.after_reindex(), vec!["keep"]);
    lib.assert_consistent();
}

#[test]
fn a_reinstall_that_cannot_write_metadata_keeps_what_was_there() {
    let lib = library();
    let src = lib.tmp.path().join("sources/keep");
    write_skill(&src, "keep");
    fs::write(src.join("extra.md"), "new file").unwrap();

    let result = crate::core::installer::install_from_local_to_destination(
        &src,
        Some("keep"),
        &lib.root.join("keep"),
    )
    .unwrap();
    assert!(!result.created_folder);
    let blocked = block_metadata();
    // The existing record is updated, then the metadata write fails.
    let _ = crate::commands::skills::store_installed_skill_unlocked(
        &lib.store,
        &result,
        &local_metadata(&src),
        None,
    );
    drop(blocked);

    assert!(lib.root.join("keep/SKILL.md").is_file(), "its folder stays");
    assert_eq!(lib.after_reindex(), vec!["keep"], "and so does its record");
    lib.assert_consistent();
}

#[test]
fn a_project_import_that_cannot_write_metadata_leaves_nothing() {
    let lib = library();
    project_with_skill(&lib, "imported");
    let (agent, rel) = project_skill_args(&lib, "imported");

    let blocked = block_metadata();
    let err =
        crate::commands::projects::import_project_skill_blocking(&lib.store, "p", &rel, &agent);
    drop(blocked);

    assert!(err.is_err());
    assert_eq!(lib.entries(), vec!["keep"]);
    assert_eq!(lib.after_reindex(), vec!["keep"]);
}

#[test]
fn a_scan_import_records_the_skill() {
    let lib = library();
    let src = lib.tmp.path().join("agent/found");
    write_skill(&src, "found");

    crate::commands::scan::import_existing_skill_unlocked(
        &lib.store,
        src.to_string_lossy().to_string(),
        None,
    )
    .unwrap();

    assert_eq!(lib.after_reindex(), vec!["found", "keep"]);
    lib.assert_consistent();
}

#[test]
fn a_scan_import_that_cannot_write_metadata_leaves_nothing() {
    let lib = library();
    let src = lib.tmp.path().join("agent/found");
    write_skill(&src, "found");

    let blocked = block_metadata();
    let err = crate::commands::scan::import_existing_skill_unlocked(
        &lib.store,
        src.to_string_lossy().to_string(),
        None,
    );
    drop(blocked);

    assert!(err.is_err());
    assert_eq!(lib.entries(), vec!["keep"]);
    assert_eq!(lib.after_reindex(), vec!["keep"]);
}

fn discovered(lib: &Library, names: &[&str]) {
    for (i, name) in names.iter().enumerate() {
        let src = lib.tmp.path().join("agent").join(name);
        write_skill(&src, name);
        lib.store
            .insert_discovered(&crate::core::skill_store::DiscoveredSkillRecord {
                id: format!("d{i}"),
                tool: "claude_code".to_string(),
                found_path: src.to_string_lossy().to_string(),
                name_guess: Some(name.to_string()),
                fingerprint: None,
                found_at: i as i64,
                imported_skill_id: None,
            })
            .unwrap();
    }
}

#[test]
fn import_all_records_each_skill_it_places() {
    let lib = library();
    discovered(&lib, &["one", "two"]);

    crate::commands::scan::import_all_discovered_unlocked(&lib.store).unwrap();

    assert_eq!(lib.after_reindex(), vec!["keep", "one", "two"]);
    lib.assert_consistent();
}

#[test]
fn import_all_that_cannot_write_metadata_leaves_no_folder_behind() {
    let lib = library();
    discovered(&lib, &["one", "two"]);

    let blocked = block_metadata();
    let err = crate::commands::scan::import_all_discovered_unlocked(&lib.store);
    drop(blocked);

    assert!(err.is_err());
    assert_eq!(lib.entries(), vec!["keep"]);
    assert_eq!(lib.after_reindex(), vec!["keep"]);
}

fn agent_with_skill(lib: &Library) -> PathBuf {
    let agent_dir = lib.tmp.path().join("claude-skills");
    write_skill(&agent_dir.join("mine"), "mine");
    lib.store
        .set_setting(
            "custom_tool_paths",
            &serde_json::json!({ "claude_code": agent_dir.to_string_lossy() }).to_string(),
        )
        .unwrap();
    agent_dir
}

#[test]
fn an_agent_import_records_the_skill() {
    let lib = library();
    agent_with_skill(&lib);

    crate::commands::agent_workspace::import_agent_local_skill_to_center(
        &lib.store,
        "claude_code",
        "mine",
    )
    .unwrap();

    assert_eq!(lib.after_reindex(), vec!["keep", "mine"]);
    lib.assert_consistent();
}

#[test]
fn an_agent_import_that_cannot_write_metadata_leaves_nothing() {
    let lib = library();
    let agent_dir = agent_with_skill(&lib);

    let blocked = block_metadata();
    let err = crate::commands::agent_workspace::import_agent_local_skill_to_center(
        &lib.store,
        "claude_code",
        "mine",
    );
    drop(blocked);

    assert!(err.is_err());
    assert_eq!(lib.entries(), vec!["keep"]);
    assert_eq!(lib.after_reindex(), vec!["keep"]);
    assert!(
        agent_dir.join("mine/SKILL.md").is_file(),
        "the agent's copy is untouched"
    );
}

#[test]
fn a_fork_records_the_copy() {
    let lib = library();
    crate::core::watched_upstreams::fork_skill(&lib.store, "keep", "kept-fork").unwrap();
    assert_eq!(lib.after_reindex(), vec!["keep", "kept-fork"]);
    lib.assert_consistent();
}

#[test]
fn a_fork_that_cannot_write_metadata_leaves_nothing() {
    let lib = library();
    let blocked = block_metadata();
    let err = crate::core::watched_upstreams::fork_skill(&lib.store, "keep", "kept-fork");
    drop(blocked);

    assert!(err.is_err());
    assert_eq!(lib.entries(), vec!["keep"]);
    assert_eq!(lib.after_reindex(), vec!["keep"]);
}

// ── update swap ──

/// `keep` as a local skill whose source now has different content.
fn reimportable(lib: &Library) -> PathBuf {
    let src = lib.tmp.path().join("sources/keep");
    write_skill(&src, "keep");
    fs::write(
        src.join("SKILL.md"),
        "---\nname: keep\ndescription: v2\n---\n\nNew body\n",
    )
    .unwrap();
    lib.store
        .update_skill_source_ref("keep", &src.to_string_lossy())
        .unwrap();
    src
}

#[test]
fn an_update_whose_old_copy_cannot_be_removed_still_records_the_new_content() {
    let lib = library();
    reimportable(&lib);
    let _armed = crate::core::fail_points::arm("swap.remove_backup");

    crate::commands::skills::reimport_local_skill_internal(&lib.store, "keep", None)
        .expect("the new content is in place, so the update succeeded");

    assert!(keep_skill_md(&lib).contains("New body"));
    let skill = lib.store.get_skill_by_id("keep").unwrap().unwrap();
    let on_disk = crate::core::content_hash::hash_directory(&lib.root.join("keep")).unwrap();
    assert_eq!(
        skill.content_hash.as_deref(),
        Some(on_disk.as_str()),
        "the record describes the new content"
    );
    // The old copy the OS would not let go of stays, hidden, next to it.
    let leftovers: Vec<String> = lib.entries().into_iter().filter(|n| n != "keep").collect();
    assert_eq!(leftovers.len(), 1);
    assert!(leftovers[0].starts_with(".keep.backup-"), "{leftovers:?}");
    assert_eq!(lib.after_reindex(), vec!["keep"]);
}

#[test]
fn an_update_whose_rename_fails_keeps_the_old_copy() {
    let lib = library();
    let before = keep_skill_md(&lib);
    reimportable(&lib);
    let _armed = crate::core::fail_points::arm("swap.rename");

    let err = crate::commands::skills::reimport_local_skill_internal(&lib.store, "keep", None);

    assert!(err.is_err());
    assert_eq!(keep_skill_md(&lib), before);
    assert_eq!(lib.entries(), vec!["keep"], "no staged or backup folder left");
    assert_eq!(lib.after_reindex(), vec!["keep"]);
}
