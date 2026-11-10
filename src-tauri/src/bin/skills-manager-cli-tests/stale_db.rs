//! sm-69q.18: the CLI's `git restore`, `git clone` and `git pull` change the
//! library on disk, and the app (another process, the same database) rewrites
//! the metadata from that database on its next write. Unless the CLI reindexed
//! inside the lock that changed the disk, the app's rewrite runs on a database
//! that does not know the skills the operation brought in, and
//! `remove_stale_metadata_files` deletes their records.
//!
//! Each case runs as a probe in a child test process (the library root is
//! process-wide state). The app is stood in for by a second `SkillStore` on the
//! same database file doing what its auto-backup does first:
//! `write_all_from_db`.

use super::*;
use std::fs;
use std::process::Command;
use tempfile::tempdir;

const PROBE_ENV: &str = "SKILLS_MANAGER_CLI_STALE_DB_PROBE";

fn git_in(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.email=t@t", "-c", "user.name=t", "-c", "core.autocrlf=false"])
        .args(args)
        .output()
        .expect("git must be runnable");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn run_probe(probe: &str) {
    let root = tempdir().unwrap();
    let name = format!("{}::{probe}", module_path!().split_once("::").unwrap().1);
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", &name, "--ignored", "--nocapture", "--test-threads=1"])
        .env(PROBE_ENV, root.path())
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && stdout.contains("1 passed"),
        "{stdout}{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn probe_root() -> Option<PathBuf> {
    std::env::var_os(PROBE_ENV).map(PathBuf::from)
}

/// A scratch library at `root/<name>`, opened the way the CLI opens one.
fn open_library(root: &Path, name: &str) -> Arc<SkillStore> {
    let lib = root.join(name);
    central_repo::set_runtime_base_dir_override(Some(lib.join("base")));
    central_repo::set_runtime_skills_dir_override(Some(lib.join("skills")));
    app_state::initialize_cli_store().unwrap()
}

/// The running app: its own connection to the library's database.
fn app_store() -> SkillStore {
    SkillStore::new(&central_repo::db_path()).unwrap()
}

/// A skill added the app's way: folder, record, metadata.
fn add_skill(store: &SkillStore, id: &str) {
    let dir = central_repo::skills_dir().join(id);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("SKILL.md"), format!("---\nname: {id}\n---\nbody\n")).unwrap();
    let now = 1;
    store
        .insert_skill(&app_lib::core::skill_store::SkillRecord {
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
    sync_metadata::write_all_from_db(store).unwrap();
}

fn record_file(id: &str) -> PathBuf {
    central_repo::skills_dir()
        .join(".skills-manager")
        .join("skills")
        .join(format!("{id}.json"))
}

fn git(command: GitCommand, store: &SkillStore) {
    run_git(GitArgs { command }, store, false, true).unwrap();
}

/// A library that backs up to a bare remote, with `ids` installed and pushed.
fn library_with_remote(root: &Path, name: &str, ids: &[&str]) -> (Arc<SkillStore>, PathBuf) {
    let remote = root.join("remote.git");
    if !remote.exists() {
        git_in(root, &["init", "-q", "--bare", "-b", "main", remote.to_str().unwrap()]);
    }
    let store = open_library(root, name);
    git(GitCommand::Init, &store);
    for id in ids {
        add_skill(&store, id);
    }
    let skills = central_repo::skills_dir();
    git_backup::commit_all(&skills, "skills").unwrap();
    git_in(&skills, &["remote", "add", "origin", remote.to_str().unwrap()]);
    git_in(&skills, &["push", "-q", "-u", "origin", "HEAD:main"]);
    (store, remote)
}

// ── git restore ──

#[test]
fn git_restore_leaves_the_restored_skills_with_their_records() {
    run_probe("git_restore_probe");
}

#[test]
#[ignore = "run by git_restore_leaves_the_restored_skills_with_their_records in a child process"]
fn git_restore_probe() {
    let Some(root) = probe_root() else { return };
    let store = open_library(&root, "lib");
    git(GitCommand::Init, &store);
    add_skill(&store, "kept");
    add_skill(&store, "returning");
    let skills = central_repo::skills_dir();
    git_backup::commit_all(&skills, "two skills").unwrap();
    let tag = git_backup::create_snapshot_tag(&skills).unwrap();

    // The skill is removed (folder, record, metadata) and that is backed up.
    fs::remove_dir_all(skills.join("returning")).unwrap();
    store.delete_skill("returning").unwrap();
    sync_metadata::write_all_from_db(&store).unwrap();
    git_backup::commit_all(&skills, "removed").unwrap();
    assert!(!record_file("returning").exists());

    // `skills-manager-cli git restore <tag>` brings it back on disk...
    git(GitCommand::Restore { tag }, &store);
    assert!(skills.join("returning").join("SKILL.md").is_file());
    assert!(record_file("returning").is_file());

    // ...and the app's next metadata write must not take its record away.
    sync_metadata::write_all_from_db(&app_store()).unwrap();
    assert!(
        record_file("returning").is_file(),
        "the restored skill's record was deleted by the app's next write"
    );
    assert!(app_store().get_skill_by_id("returning").unwrap().is_some());
    assert!(record_file("kept").is_file());
}

// ── git clone ──

#[test]
fn git_clone_leaves_the_cloned_skills_with_their_records() {
    run_probe("git_clone_probe");
}

#[test]
#[ignore = "run by git_clone_leaves_the_cloned_skills_with_their_records in a child process"]
fn git_clone_probe() {
    let Some(root) = probe_root() else { return };
    let (_a, remote) = library_with_remote(&root, "a", &["from-a"]);

    // Device B: a fresh library, connected by `skills-manager-cli git clone`.
    let b = open_library(&root, "b");
    git(GitCommand::Clone { url: remote.to_string_lossy().to_string() }, &b);
    assert!(record_file("from-a").is_file());

    sync_metadata::write_all_from_db(&app_store()).unwrap();
    assert!(
        record_file("from-a").is_file(),
        "the cloned skill's record was deleted by the app's next write"
    );
    assert!(app_store().get_skill_by_id("from-a").unwrap().is_some());
}

// ── git pull ──

#[test]
fn git_pull_reindexes_inside_the_lock_that_pulled() {
    run_probe("git_pull_probe");
}

/// The repo lock stamps the operation that holds it into the lock file, and
/// the file keeps the last stamp. A pull that reindexes under a lock of its
/// own leaves "reindex sync metadata" there; between the two locks the app
/// can take the lock and write the metadata from the pre-pull database,
/// deleting the records of the skills the pull brought in.
#[test]
#[ignore = "run by git_pull_reindexes_inside_the_lock_that_pulled in a child process"]
fn git_pull_probe() {
    let Some(root) = probe_root() else { return };
    let (a, remote) = library_with_remote(&root, "a", &["first"]);

    let b = open_library(&root, "b");
    git(GitCommand::Clone { url: remote.to_string_lossy().to_string() }, &b);
    sync_metadata::reindex_from_metadata(&b).unwrap();

    // Device A adds a skill and backs it up.
    let _ = open_library(&root, "a");
    add_skill(&a, "second");
    let a_skills = central_repo::skills_dir();
    git_backup::commit_all(&a_skills, "second").unwrap();
    git_in(&a_skills, &["push", "-q", "origin", "HEAD:main"]);

    // Device B pulls through the CLI.
    let _ = open_library(&root, "b");
    git(GitCommand::Pull, &b);
    assert!(record_file("second").is_file());
    assert!(b.get_skill_by_id("second").unwrap().is_some());

    let stamp = fs::read_to_string(central_repo::base_dir().join(".skills-manager.lock")).unwrap();
    assert!(
        stamp.contains("operation=git pull"),
        "the pull released its lock before the reindex: {stamp}"
    );

    sync_metadata::write_all_from_db(&app_store()).unwrap();
    assert!(record_file("second").is_file());
}
