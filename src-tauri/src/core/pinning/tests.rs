//! Pins and upstream commits (sm-69q.5).
//!
//! The update command refuses `file://` URLs, so the end-to-end test runs in
//! a child test process whose git is told, through `GIT_CONFIG_*` in the
//! child's environment only, to fetch a made-up https host from a local
//! repository (the same arrangement as the baseline tests).

use super::*;
use crate::commands::skills as cmd;
use crate::core::central_repo;
use crate::core::repo_lock::RepoLock;
use crate::core::{installer, skill_auto_updater};
use std::fs;
use std::process::Command;
use std::sync::MutexGuard;
use tempfile::{tempdir, TempDir};

struct TestRepo {
    _lock: MutexGuard<'static, ()>,
    tmp: TempDir,
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
        tmp,
        store,
    }
}

fn git_in(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.email=t@t", "-c", "user.name=t"])
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

/// `file://` wants forward slashes and, on Windows, a slash before the drive.
fn file_url(path: &Path) -> String {
    let raw = path.display().to_string().replace('\\', "/");
    if raw.starts_with('/') {
        format!("file://{raw}")
    } else {
        format!("file:///{raw}")
    }
}

fn write(dir: &Path, rel: &str, body: &str) {
    let path = dir.join(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, body).unwrap();
}

fn commit(dir: &Path, rel: &str, body: &str, subject: &str) -> String {
    write(dir, rel, body);
    git_in(dir, &["add", "-A"]);
    git_in(dir, &["commit", "-qm", subject]);
    git_in(dir, &["rev-parse", "HEAD"])
}

/// An upstream with `skills/a` and `skills/b`. Returns its path and first commit.
fn upstream(root: &Path) -> (PathBuf, String) {
    let dir = root.join("up");
    write(&dir, "skills/a/SKILL.md", "---\nname: a\n---\nv1\n");
    write(&dir, "skills/b/SKILL.md", "---\nname: b\n---\nv1\n");
    git_in(&dir, &["init", "-q", "-b", "main"]);
    git_in(&dir, &["config", "uploadpack.allowFilter", "true"]);
    git_in(&dir, &["add", "-A"]);
    git_in(&dir, &["commit", "-qm", "init"]);
    let first = git_in(&dir, &["rev-parse", "HEAD"]);
    (dir, first)
}

fn git_skill(
    id: &str,
    url: &str,
    subpath: Option<&str>,
    revision: Option<&str>,
    source_type: &str,
) -> SkillRecord {
    let dir = central_repo::skills_dir().join(id);
    write(&dir, "SKILL.md", &format!("---\nname: {id}\n---\n"));
    SkillRecord {
        id: id.to_string(),
        name: id.to_string(),
        description: None,
        source_type: source_type.to_string(),
        source_ref: Some(url.to_string()),
        source_ref_resolved: Some(url.to_string()),
        source_subpath: subpath.map(str::to_string),
        source_branch: None,
        source_revision: revision.map(str::to_string),
        remote_revision: revision.map(str::to_string),
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

const REV1: &str = "1111111111111111111111111111111111111111";

// ── pin state ──

#[test]
fn pin_and_unpin_round_trip_through_the_fork_file() {
    let repo = test_repo();
    let skill = git_skill("g", "https://example.invalid/r.git", None, Some(REV1), "git");
    repo.store.insert_skill(&skill).unwrap();

    assert!(!holds("g"));
    let pin = pin(&repo.store, "g", Some("  reviewed v1  ")).unwrap();
    assert!(pin.pinned);
    assert_eq!(pin.revision.as_deref(), Some(REV1));
    assert_eq!(pin.note.as_deref(), Some("reviewed v1"));
    assert!(holds("g"));
    assert_eq!(active("g"), Some(pin.clone()));
    let path = fork_files::path_for(ForkConcern::Pin, "g").unwrap();
    assert!(path.ends_with(".skills-manager/fork-pin.g.json"));
    assert!(!Path::new(&skill.central_path).join("fork-pin.g.json").exists());

    // Pinning again keeps the revision and time; a new note replaces the old.
    repo.store
        .update_skill_source_metadata("g", None, None, None, Some(&"2".repeat(40)))
        .unwrap();
    let again = super::pin(&repo.store, "g", Some("still")).unwrap();
    assert_eq!(again.revision.as_deref(), Some(REV1));
    assert_eq!(again.pinned_at, pin.pinned_at);
    assert_eq!(again.note.as_deref(), Some("still"));
    let kept = super::pin(&repo.store, "g", None).unwrap();
    assert_eq!(kept.note.as_deref(), Some("still"));

    let released = unpin(&repo.store, "g").unwrap();
    assert!(!released.pinned);
    assert!(!holds("g"));
    assert_eq!(active("g"), None);
    // Unpinning writes `pinned: false`, so it syncs as a modification.
    assert_eq!(load("g").unwrap().map(|s| s.pinned), Some(false));
}

#[test]
fn unpinning_a_skill_never_pinned_writes_nothing() {
    let repo = test_repo();
    let skill = git_skill("g", "https://example.invalid/r.git", None, Some(REV1), "git");
    repo.store.insert_skill(&skill).unwrap();
    assert!(!unpin(&repo.store, "g").unwrap().pinned);
    assert!(!fork_files::path_for(ForkConcern::Pin, "g").unwrap().exists());
}

#[test]
fn only_git_skills_can_be_pinned() {
    let repo = test_repo();
    let mut skill = git_skill("l", "/somewhere", None, None, "local");
    skill.source_ref_resolved = None;
    repo.store.insert_skill(&skill).unwrap();
    let err = pin(&repo.store, "l", None).unwrap_err();
    assert!(err.message.contains("Only git"), "{}", err.message);
    assert!(!holds("l"));
    assert!(pin(&repo.store, "missing", None).is_err());
}

#[test]
fn an_unreadable_pin_holds_the_skill_until_unpinned() {
    let repo = test_repo();
    let skill = git_skill("g", "https://example.invalid/r.git", None, Some(REV1), "git");
    repo.store.insert_skill(&skill).unwrap();
    let path = fork_files::path_for(ForkConcern::Pin, "g").unwrap();
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, "{ not json").unwrap();

    assert!(holds("g"));
    assert!(refuse_update_if_pinned(&skill).is_err());
    unpin(&repo.store, "g").unwrap();
    assert!(!holds("g"));
}

#[test]
fn a_pinned_skill_refuses_an_update_and_names_unpin() {
    let repo = test_repo();
    let skill = git_skill("g", "https://example.invalid/r.git", None, Some(REV1), "git");
    repo.store.insert_skill(&skill).unwrap();
    pin(&repo.store, "g", None).unwrap();

    // Refused before any network: the bogus host is never contacted.
    let err = cmd::update_git_skill_internal(&repo.store, "g", None, None, None).unwrap_err();
    assert!(err.message.contains("is pinned at 1111111"), "{}", err.message);
    assert!(err.message.contains("skills unpin g"), "{}", err.message);
    let row = repo.store.get_skill_by_id("g").unwrap().unwrap();
    assert_eq!(row.source_revision.as_deref(), Some(REV1));
    assert_eq!(row.update_status, "up_to_date");
}

#[test]
fn the_skill_list_carries_the_pin() {
    let repo = test_repo();
    let skill = git_skill("g", "https://example.invalid/r.git", None, Some(REV1), "git");
    repo.store.insert_skill(&skill).unwrap();
    assert!(cmd::managed_skill_by_id(&repo.store, "g").unwrap().pin.is_none());
    pin(&repo.store, "g", Some("n")).unwrap();
    let dto = cmd::managed_skill_by_id(&repo.store, "g").unwrap();
    assert_eq!(dto.pin.and_then(|p| p.note).as_deref(), Some("n"));
}

// ── upstream commits ──

#[test]
fn commits_lists_only_commits_that_touch_the_skill_directory() {
    let repo = test_repo();
    let (up, first) = upstream(repo.tmp.path());
    let url = file_url(&up);
    let a1 = commit(&up, "skills/a/SKILL.md", "---\nname: a\n---\nv2\n", "a: second");
    commit(&up, "skills/b/SKILL.md", "---\nname: b\n---\nv2\n", "b: only b");
    commit(&up, "README.md", "root\n", "root readme");
    let a2 = commit(&up, "skills/a/notes.md", "notes\n", "a: notes");

    let skill = git_skill("a", &url, Some("skills/a"), Some(&first), "git");
    repo.store.insert_skill(&skill).unwrap();
    let listed = upstream_commits_for(&skill, None, false).unwrap();
    assert!(listed.reachable, "{:?}", listed.message);
    assert_eq!(listed.base_from, "source_revision");
    assert_eq!(listed.base_revision, first);
    assert_eq!(listed.remote_revision, a2);
    assert_eq!(listed.subpath.as_deref(), Some("skills/a"));
    let shas: Vec<&str> = listed.commits.iter().map(|c| c.sha.as_str()).collect();
    assert_eq!(shas, vec![a2.as_str(), a1.as_str()], "newest first, a only");
    assert_eq!(listed.commits[0].subject, "a: notes");
    assert_eq!(listed.commits[0].author, "t");
    assert!(chrono::DateTime::parse_from_rfc3339(&listed.commits[0].date).is_ok());
    assert!(!listed.truncated);

    // A Windows-style subpath from old metadata filters the same way.
    let mut backslash = skill.clone();
    backslash.source_subpath = Some("skills\\b".to_string());
    backslash.id = "b".to_string();
    let b = upstream_commits_for(&backslash, None, false).unwrap();
    assert_eq!(b.commits.len(), 1);
    assert_eq!(b.commits[0].subject, "b: only b");

    // Up to date: nothing to list, nothing fetched.
    let mut current = skill.clone();
    current.source_revision = Some(a2.clone());
    let none = upstream_commits_for(&current, None, false).unwrap();
    assert!(none.reachable && none.commits.is_empty());
}

#[test]
fn commits_start_from_the_baseline_when_there_is_one() {
    let repo = test_repo();
    let (up, first) = upstream(repo.tmp.path());
    let url = file_url(&up);
    let a1 = commit(&up, "skills/a/SKILL.md", "---\nname: a\n---\nv2\n", "a: second");
    let a2 = commit(&up, "skills/a/SKILL.md", "---\nname: a\n---\nv3\n", "a: third");

    // The baseline says a1; `source_revision` (moved by check, #561) says first.
    let skill = git_skill("a", &url, Some("skills/a"), Some(&first), "git");
    repo.store.insert_skill(&skill).unwrap();
    let staged = repo.tmp.path().join("staged");
    write(&staged, "SKILL.md", "---\nname: a\n---\nv2\n");
    {
        let _lock = RepoLock::acquire_foreground("test").unwrap();
        baseline::record_unlocked("a", &a1, baseline::Origin::Install, &staged).unwrap();
    }
    let listed = upstream_commits_for(&skill, None, false).unwrap();
    assert_eq!(listed.base_from, "baseline");
    assert_eq!(listed.base_revision, a1);
    let shas: Vec<&str> = listed.commits.iter().map(|c| c.sha.as_str()).collect();
    assert_eq!(shas, vec![a2.as_str()]);
}

#[test]
fn commits_say_plainly_when_the_base_is_not_reachable() {
    let repo = test_repo();
    let (up, _first) = upstream(repo.tmp.path());
    let url = file_url(&up);

    // Gone upstream: the server never had it.
    let gone = "0123456789abcdef0123456789abcdef01234567";
    let skill = git_skill("a", &url, Some("skills/a"), Some(gone), "git");
    repo.store.insert_skill(&skill).unwrap();
    let listed = upstream_commits_for(&skill, None, false).unwrap();
    assert!(!listed.reachable);
    assert!(listed.commits.is_empty());
    assert!(
        listed.message.as_deref().unwrap().contains("no longer available upstream"),
        "{:?}",
        listed.message
    );

    // On a side branch the server still has, but not in the head's history.
    git_in(&up, &["checkout", "-qb", "side"]);
    let side = commit(&up, "skills/a/SKILL.md", "---\nname: a\n---\nside\n", "side");
    git_in(&up, &["checkout", "-q", "main"]);
    commit(&up, "skills/a/SKILL.md", "---\nname: a\n---\nmain\n", "main");
    let mut off = skill.clone();
    off.source_revision = Some(side);
    let listed = upstream_commits_for(&off, None, false).unwrap();
    assert!(!listed.reachable);
    assert!(
        listed.message.as_deref().unwrap().contains("not in the history"),
        "{:?}",
        listed.message
    );
}

#[test]
fn a_merge_that_only_brings_in_a_side_change_is_not_listed_twice() {
    let repo = test_repo();
    let (up, first) = upstream(repo.tmp.path());
    let url = file_url(&up);
    git_in(&up, &["checkout", "-qb", "side"]);
    let side = commit(&up, "skills/a/side.md", "side\n", "a: on a branch");
    git_in(&up, &["checkout", "-q", "main"]);
    commit(&up, "skills/b/SKILL.md", "---\nname: b\n---\nv2\n", "b: main");
    git_in(&up, &["merge", "-q", "--no-ff", "-m", "merge side", "side"]);

    let skill = git_skill("a", &url, Some("skills/a"), Some(&first), "git");
    repo.store.insert_skill(&skill).unwrap();
    let listed = upstream_commits_for(&skill, None, false).unwrap();
    let subjects: Vec<&str> = listed.commits.iter().map(|c| c.subject.as_str()).collect();
    assert_eq!(subjects, vec!["a: on a branch"]);
    assert_eq!(listed.commits[0].sha, side);
}

// ── end to end: install, pin, check, update, auto-update, unpin ──

const PROBE_ENV: &str = "SKILLS_MANAGER_PIN_PROBE";
const FAKE_HOST: &str = "https://pin-test.invalid/";

#[test]
fn a_pinned_skill_is_checked_but_never_updated_until_unpinned() {
    let root = tempdir().unwrap();
    let name = concat!(module_path!(), "::end_to_end_probe")
        .split_once("::")
        .unwrap()
        .1;
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", name, "--ignored", "--nocapture", "--test-threads=1"])
        .env(PROBE_ENV, root.path())
        .env("GIT_CONFIG_COUNT", "1")
        .env(
            "GIT_CONFIG_KEY_0",
            format!("url.{}/.insteadOf", file_url(root.path())),
        )
        .env("GIT_CONFIG_VALUE_0", FAKE_HOST)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && stdout.contains("1 passed"),
        "{stdout}{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
#[ignore = "run by a_pinned_skill_is_checked_but_never_updated_until_unpinned in a child process"]
fn end_to_end_probe() {
    let Some(root) = std::env::var_os(PROBE_ENV).map(PathBuf::from) else {
        return;
    };
    let repo = test_repo();
    let (up, rev1) = upstream(&root);
    let url = format!("{FAKE_HOST}up");

    // Install `skills/a` at rev1, as the install command does.
    let checkout =
        git_fetcher::clone_repo_ref_scoped(&url, None, Some("skills/a"), None, None, None).unwrap();
    git_fetcher::checkout_revision(&checkout, &rev1).unwrap();
    let dest = central_repo::skills_dir().join("a");
    let result =
        installer::install_skill_dir_to_destination(&checkout.join("skills/a"), "a", &dest)
            .unwrap();
    git_fetcher::cleanup_temp(&checkout);
    let meta = cmd::InstallSourceMetadata {
        source_type: "git".to_string(),
        source_ref: Some(url.clone()),
        source_ref_resolved: Some(url.clone()),
        source_subpath: Some("skills/a".to_string()),
        source_branch: None,
        source_revision: Some(rev1.clone()),
        remote_revision: Some(rev1.clone()),
        update_status: "up_to_date".to_string(),
    };
    let id = {
        let _lock = RepoLock::acquire_foreground("test").unwrap();
        cmd::store_installed_skill_unlocked(&repo.store, &result, &meta, None).unwrap()
    };
    let installed_body = fs::read_to_string(dest.join("SKILL.md")).unwrap();

    pin(&repo.store, &id, Some("reviewed")).unwrap();
    let rev2 = commit(&up, "skills/a/SKILL.md", "---\nname: a\n---\nv2\n", "a: v2");

    // Check still resolves and records the newer remote.
    let dto = cmd::check_skill_update_internal(&repo.store, &id, true, None).unwrap();
    assert_eq!(dto.update_status, "update_available");
    assert!(dto.pin.is_some());
    let row = repo.store.get_skill_by_id(&id).unwrap().unwrap();
    assert_eq!(row.remote_revision.as_deref(), Some(rev2.as_str()));
    assert_eq!(row.source_revision.as_deref(), Some(rev1.as_str()));

    // The commit list goes from the install baseline to the new head.
    let listed = upstream_commits(&repo.store, &id, None).unwrap();
    assert_eq!(listed.base_from, "baseline");
    assert_eq!(
        listed.commits.iter().map(|c| c.sha.clone()).collect::<Vec<_>>(),
        vec![rev2.clone()]
    );

    // A hand update is refused.
    let err = cmd::update_git_skill_internal(&repo.store, &id, None, None, None).unwrap_err();
    assert!(err.message.contains("unpin"), "{}", err.message);

    // An auto-update round that may apply updates checks it, skips it and
    // reports it.
    repo.store.set_setting("auto_update_apply", "on").unwrap();
    let round = skill_auto_updater::run_round_blocking(&repo.store).unwrap();
    assert_eq!(round.pinned, 1);
    assert_eq!(round.updated, 0);
    let row = repo.store.get_skill_by_id(&id).unwrap().unwrap();
    assert_eq!(row.source_revision.as_deref(), Some(rev1.as_str()));
    assert_eq!(row.update_status, "update_available");
    assert_eq!(fs::read_to_string(dest.join("SKILL.md")).unwrap(), installed_body);

    // Unpinned, the same update is an ordinary one.
    unpin(&repo.store, &id).unwrap();
    let updated = cmd::update_git_skill_internal(&repo.store, &id, None, None, None).unwrap();
    assert!(updated.content_changed);
    let row = repo.store.get_skill_by_id(&id).unwrap().unwrap();
    assert_eq!(row.source_revision.as_deref(), Some(rev2.as_str()));
    assert!(fs::read_to_string(dest.join("SKILL.md")).unwrap().contains("v2"));
    assert_eq!(baseline::load(&id).unwrap().unwrap().revision, rev2);
}
