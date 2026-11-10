//! Overwrite holds (sm-69q.4).
//!
//! The update command refuses `file://` URLs, so the end-to-end tests run in
//! a child test process whose git is told, through `GIT_CONFIG_*` in the
//! child's environment only, to fetch a made-up https host from a local
//! repository (the same arrangement as the baseline and pinning tests).

use super::*;
use crate::commands::skills as cmd;
use crate::core::baseline::{self, record, Origin};
use crate::core::central_repo;
use crate::core::repo_lock::RepoLock;
use crate::core::skill_store::PendingConflictRow;
use crate::core::{content_hash, installer, skill_auto_updater};
use std::fs;
use std::path::PathBuf;
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

fn write(dir: &Path, rel: &str, body: &str) {
    let path = dir.join(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, body).unwrap();
}

fn write_files(dir: &Path, files: &[(&str, &str)]) {
    for (rel, body) in files {
        write(dir, rel, body);
    }
}

const REV1: &str = "1111111111111111111111111111111111111111";

fn skill_record(id: &str, dir: &Path) -> SkillRecord {
    SkillRecord {
        id: id.to_string(),
        name: id.to_string(),
        description: None,
        source_type: "git".to_string(),
        source_ref: Some("https://example.invalid/repo.git".to_string()),
        source_ref_resolved: Some("https://example.invalid/repo.git".to_string()),
        source_subpath: None,
        source_branch: None,
        source_revision: Some(REV1.to_string()),
        remote_revision: Some(REV1.to_string()),
        central_path: dir.to_string_lossy().to_string(),
        content_hash: Some(content_hash::hash_directory(dir).unwrap()),
        enabled: true,
        created_at: 1,
        updated_at: 1,
        status: "ok".to_string(),
        update_status: "up_to_date".to_string(),
        last_checked_at: None,
        last_check_error: None,
    }
}

/// A git skill in the library holding `files`, baselined from exactly those.
fn baselined_skill(repo: &TestRepo, id: &str, files: &[(&str, &str)]) -> SkillRecord {
    let dir = central_repo::skills_dir().join(id);
    write_files(&dir, files);
    let skill = skill_record(id, &dir);
    repo.store.insert_skill(&skill).unwrap();
    record(id, REV1, Origin::Install, &dir).unwrap();
    skill
}

fn staged(repo: &TestRepo, files: &[(&str, &str)]) -> PathBuf {
    let dir = repo.tmp.path().join(format!("staged-{}", uuid::Uuid::new_v4()));
    write_files(&dir, files);
    dir
}

fn paths(check: &OverwriteCheck) -> Vec<(&str, Option<FileChangeState>)> {
    check
        .files
        .iter()
        .map(|f| (f.path.as_str(), f.local))
        .collect()
}

// ── the comparison ──

#[test]
fn a_clean_skill_has_nothing_to_hold_whatever_upstream_changed() {
    let repo = test_repo();
    let skill = baselined_skill(&repo, "demo", &[("SKILL.md", "v1\n"), ("notes.md", "n1\n")]);
    let up = staged(&repo, &[("SKILL.md", "v2\n"), ("new.md", "new\n")]);
    let check = pending_overwrites(&repo.store, &skill, &up).unwrap();
    assert!(check.is_empty(), "{check:?}");
    assert_eq!(check.reason, None);
}

#[test]
fn an_edited_file_upstream_also_changed_is_held() {
    let repo = test_repo();
    let skill = baselined_skill(&repo, "demo", &[("SKILL.md", "v1\n"), ("notes.md", "n1\n")]);
    write(Path::new(&skill.central_path), "SKILL.md", "mine\n");
    let up = staged(&repo, &[("SKILL.md", "v2\n"), ("notes.md", "n1\n")]);
    let check = pending_overwrites(&repo.store, &skill, &up).unwrap();
    assert_eq!(check.reason, Some(OverwriteReason::LocalChanges));
    assert_eq!(paths(&check), vec![("SKILL.md", Some(FileChangeState::Modified))]);
    assert_eq!(check.files[0].location, "library");
}

#[test]
fn an_edited_file_upstream_left_alone_is_held_until_merging_exists() {
    // Design §4 "unchanged / modified: keep local" is not "keep", so until
    // child .6 it is a hold: the swap would put upstream's old copy back.
    let repo = test_repo();
    let skill = baselined_skill(&repo, "demo", &[("SKILL.md", "v1\n"), ("notes.md", "n1\n")]);
    write(Path::new(&skill.central_path), "SKILL.md", "mine\n");
    let up = staged(&repo, &[("SKILL.md", "v1\n"), ("notes.md", "n2\n")]);
    let check = pending_overwrites(&repo.store, &skill, &up).unwrap();
    assert_eq!(paths(&check), vec![("SKILL.md", Some(FileChangeState::Modified))]);
}

#[test]
fn an_edit_that_already_matches_upstream_is_not_an_overwrite() {
    let repo = test_repo();
    let skill = baselined_skill(&repo, "demo", &[("SKILL.md", "v1\n")]);
    // Same content, different line endings: still the same file.
    write(Path::new(&skill.central_path), "SKILL.md", "v2\r\n");
    let up = staged(&repo, &[("SKILL.md", "v2\n")]);
    assert!(pending_overwrites(&repo.store, &skill, &up).unwrap().is_empty());
}

#[test]
fn deleted_and_added_files_are_held_and_library_only_files_are_left_to_removals() {
    let repo = test_repo();
    let skill = baselined_skill(
        &repo,
        "demo",
        &[("SKILL.md", "v1\n"), ("gone.md", "g\n"), ("kept.md", "k\n")],
    );
    let dir = Path::new(&skill.central_path);
    fs::remove_file(dir.join("gone.md")).unwrap();
    write(dir, "clash.md", "mine\n");
    write(dir, "extra.md", "only mine\n");
    let up = staged(
        &repo,
        &[
            ("SKILL.md", "v1\n"),
            ("gone.md", "g\n"),
            ("kept.md", "k\n"),
            ("clash.md", "upstream's\n"),
        ],
    );
    let check = pending_overwrites(&repo.store, &skill, &up).unwrap();
    assert_eq!(
        paths(&check),
        vec![
            ("clash.md", Some(FileChangeState::Added)),
            ("gone.md", Some(FileChangeState::Deleted)),
        ]
    );
}

#[test]
fn an_unknown_baseline_holds_every_file_the_update_would_change() {
    let repo = test_repo();
    let skill = baselined_skill(&repo, "demo", &[("SKILL.md", "v1\n"), ("same.md", "s\n")]);
    baseline::forget(&skill.id).unwrap();
    let up = staged(
        &repo,
        &[("SKILL.md", "v2\n"), ("same.md", "s\n"), ("new.md", "n\n")],
    );
    let check = pending_overwrites(&repo.store, &skill, &up).unwrap();
    assert_eq!(check.reason, Some(OverwriteReason::BaselineNotBackfilled));
    assert_eq!(paths(&check), vec![("SKILL.md", None), ("new.md", None)]);

    // An update that changes nothing in the library holds nothing.
    let same = staged(&repo, &[("SKILL.md", "v1\n"), ("same.md", "s\n")]);
    assert!(pending_overwrites(&repo.store, &skill, &same).unwrap().is_empty());
}

#[test]
fn a_pending_sync_conflict_holds_every_changed_file_as_suspect() {
    let repo = test_repo();
    let skill = baselined_skill(&repo, "demo", &[("SKILL.md", "v1\n"), ("notes.md", "n1\n")]);
    repo.store
        .replace_pending_conflicts(&[PendingConflictRow {
            skill_id: "demo".to_string(),
            theirs_commit: "2".repeat(40),
            theirs_path: None,
            detected_at: 1,
        }])
        .unwrap();
    // Not edited here, but the baseline may be the other device's.
    let up = staged(&repo, &[("SKILL.md", "v2\n"), ("notes.md", "n1\n")]);
    let check = pending_overwrites(&repo.store, &skill, &up).unwrap();
    assert_eq!(check.reason, Some(OverwriteReason::BaselineSuspect));
    assert_eq!(paths(&check), vec![("SKILL.md", None)]);
}

#[test]
fn an_unreadable_staged_tree_is_an_error_never_an_empty_hold() {
    let repo = test_repo();
    let skill = baselined_skill(&repo, "demo", &[("SKILL.md", "v1\n")]);
    let missing = repo.tmp.path().join("no-such-staged-dir");
    assert!(pending_overwrites(&repo.store, &skill, &missing).is_err());
}

#[test]
fn reasons_serialize_in_snake_case() {
    for (reason, text) in [
        (OverwriteReason::LocalChanges, "local_changes"),
        (OverwriteReason::BaselineNotBackfilled, "baseline_not_backfilled"),
        (OverwriteReason::BaselineUnrecoverable, "baseline_unrecoverable"),
        (OverwriteReason::BaselineSuspect, "baseline_suspect"),
    ] {
        assert_eq!(serde_json::to_value(reason).unwrap(), text);
        assert!(!reason.describe().is_empty());
    }
}

// ── end to end: install, edit, update, approve, auto-update ──

const PROBE_ENV: &str = "SKILLS_MANAGER_OVERWRITE_PROBE";
const FAKE_HOST: &str = "https://overwrite-test.invalid/";

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

fn commit(dir: &Path, rel: &str, body: &str, subject: &str) -> String {
    write(dir, rel, body);
    git_in(dir, &["add", "-A"]);
    git_in(dir, &["commit", "-qm", subject]);
    git_in(dir, &["rev-parse", "HEAD"])
}

/// Run `probe` (an ignored test in this module) in a child process whose git
/// maps `FAKE_HOST` onto `root`.
fn run_probe(probe: &str) {
    let root = tempdir().unwrap();
    let name = format!(
        "{}::{probe}",
        module_path!().split_once("::").unwrap().1
    );
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", &name, "--ignored", "--nocapture", "--test-threads=1"])
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

/// An upstream repository `up` with one skill at its root.
fn upstream(root: &Path) -> (PathBuf, String) {
    let dir = root.join("up");
    write(&dir, "SKILL.md", "---\nname: demo\n---\nline one\nline two\n");
    write(&dir, "notes.md", "notes v1\n");
    git_in(&dir, &["init", "-q", "-b", "main"]);
    git_in(&dir, &["config", "uploadpack.allowFilter", "true"]);
    git_in(&dir, &["add", "-A"]);
    git_in(&dir, &["commit", "-qm", "init"]);
    let first = git_in(&dir, &["rev-parse", "HEAD"]);
    (dir, first)
}

/// Install the upstream skill at `rev` as `name`, as the install command does.
fn install(store: &SkillStore, url: &str, rev: &str, name: &str) -> (String, PathBuf) {
    let checkout = git_fetcher::clone_repo_ref_scoped(url, None, None, None, None, None).unwrap();
    git_fetcher::checkout_revision(&checkout, rev).unwrap();
    let dest = central_repo::skills_dir().join(name);
    let result = installer::install_skill_dir_to_destination(&checkout, name, &dest).unwrap();
    git_fetcher::cleanup_temp(&checkout);
    let meta = cmd::InstallSourceMetadata {
        source_type: "git".to_string(),
        source_ref: Some(url.to_string()),
        source_ref_resolved: Some(url.to_string()),
        source_subpath: None,
        source_branch: None,
        source_revision: Some(rev.to_string()),
        remote_revision: Some(rev.to_string()),
        update_status: "up_to_date".to_string(),
    };
    let id = {
        let _lock = RepoLock::acquire_foreground("test").unwrap();
        cmd::store_installed_skill_unlocked(store, &result, &meta, None).unwrap()
    };
    (id, dest)
}

/// Every file under `dir` with its bytes, for byte-for-byte comparisons.
fn snapshot(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    walkdir::WalkDir::new(dir)
        .into_iter()
        .flatten()
        .filter(|e| e.file_type().is_file())
        .map(|e| {
            (
                e.path().strip_prefix(dir).unwrap().display().to_string(),
                fs::read(e.path()).unwrap(),
            )
        })
        .collect()
}

/// Nothing named after a staged directory is left next to the skill.
fn no_staged_leftovers() {
    let leftovers: Vec<String> = fs::read_dir(central_repo::skills_dir())
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n.contains("staged"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
}

#[test]
fn a_manual_update_holds_local_edits_until_approved() {
    run_probe("manual_update_probe");
}

#[test]
#[ignore = "run by a_manual_update_holds_local_edits_until_approved in a child process"]
fn manual_update_probe() {
    let Some(root) = std::env::var_os(PROBE_ENV).map(PathBuf::from) else {
        return;
    };
    let repo = test_repo();
    let (up, rev1) = upstream(&root);
    let url = format!("{FAKE_HOST}up");
    let (id, dest) = install(&repo.store, &url, &rev1, "demo");

    // 1. Edit a file upstream also changes: held, nothing touched.
    write(&dest, "SKILL.md", "---\nname: demo\n---\nline one\nMINE\n");
    let rev2 = commit(&up, "SKILL.md", "---\nname: demo\n---\nline one\nline 2\n", "v2");
    let before = snapshot(&dest);
    let held = cmd::update_git_skill_internal(&repo.store, &id, None, None, None).unwrap();
    assert!(!held.content_changed);
    assert!(held.pending_removals.is_empty());
    assert_eq!(held.overwrite_reason, Some(OverwriteReason::LocalChanges));
    let held_paths: Vec<&str> = held.pending_overwrites.iter().map(|o| o.path.as_str()).collect();
    assert_eq!(held_paths, vec!["SKILL.md"]);
    assert!(held.is_held());
    let token = held.removal_approval.clone().expect("a held update carries a token");
    assert_eq!(snapshot(&dest), before, "a held update changes nothing");
    no_staged_leftovers();
    let row = repo.store.get_skill_by_id(&id).unwrap().unwrap();
    assert_eq!(row.update_status, "update_available");
    assert_eq!(row.source_revision.as_deref(), Some(rev1.as_str()));
    assert_eq!(baseline::load(&id).unwrap().unwrap().revision, rev1);

    // A wrong token, or one for another revision, does not approve.
    let wrong = cmd::update_git_skill_internal(&repo.store, &id, None, None, Some("nope")).unwrap();
    assert!(wrong.is_held());
    assert_eq!(snapshot(&dest), before);
    let rev3 = commit(&up, "notes.md", "notes v3\n", "v3");
    let stale = cmd::update_git_skill_internal(&repo.store, &id, None, None, Some(&token)).unwrap();
    assert!(stale.is_held(), "a token from another revision must not apply");
    assert_ne!(stale.removal_approval.as_deref(), Some(token.as_str()));
    assert_eq!(snapshot(&dest), before);

    // A token for a different list does not approve either.
    write(&dest, "notes.md", "my notes\n");
    let longer = cmd::update_git_skill_internal(
        &repo.store,
        &id,
        None,
        None,
        stale.removal_approval.as_deref(),
    )
    .unwrap();
    assert!(longer.is_held(), "the list changed, so the approval is stale");
    let longer_paths: Vec<&str> = longer.pending_overwrites.iter().map(|o| o.path.as_str()).collect();
    assert_eq!(longer_paths, vec!["SKILL.md", "notes.md"]);

    // The current token applies it, and the baseline moves.
    let applied = cmd::update_git_skill_internal(
        &repo.store,
        &id,
        None,
        None,
        longer.removal_approval.as_deref(),
    )
    .unwrap();
    assert!(applied.content_changed);
    assert!(!applied.is_held());
    assert!(applied.pending_overwrites.is_empty());
    assert!(fs::read_to_string(dest.join("SKILL.md")).unwrap().contains("line 2"));
    assert_eq!(fs::read_to_string(dest.join("notes.md")).unwrap(), "notes v3\n");
    assert_eq!(baseline::load(&id).unwrap().unwrap().revision, rev3);
    no_staged_leftovers();
    let _ = rev2;

    // 2. Edit one file while upstream changes another: held too, for now.
    write(&dest, "SKILL.md", "---\nname: demo\n---\nmine again\n");
    commit(&up, "notes.md", "notes v4\n", "v4");
    let before = snapshot(&dest);
    let held = cmd::update_git_skill_internal(&repo.store, &id, None, None, None).unwrap();
    let held_paths: Vec<&str> = held.pending_overwrites.iter().map(|o| o.path.as_str()).collect();
    assert_eq!(held_paths, vec!["SKILL.md"]);
    assert_eq!(snapshot(&dest), before);

    // 3. An unedited skill updates exactly as before: no prompt.
    let (other, other_dest) = install(&repo.store, &url, &git_in(&up, &["rev-parse", "HEAD"]), "plain");
    let rev5 = commit(&up, "SKILL.md", "---\nname: demo\n---\nv5\n", "v5");
    let plain = cmd::update_git_skill_internal(&repo.store, &other, None, None, None).unwrap();
    assert!(plain.content_changed);
    assert!(!plain.is_held());
    assert!(plain.removal_approval.is_none());
    assert!(plain.pending_overwrites.is_empty());
    assert_eq!(plain.overwrite_reason, None);
    assert!(fs::read_to_string(other_dest.join("SKILL.md")).unwrap().contains("v5"));
    assert_eq!(baseline::load(&other).unwrap().unwrap().revision, rev5);
}

#[test]
fn unattended_updates_and_unknown_baselines_hold_and_name_the_reason() {
    run_probe("unattended_probe");
}

#[test]
#[ignore = "run by unattended_updates_and_unknown_baselines_hold_and_name_the_reason in a child process"]
fn unattended_probe() {
    let Some(root) = std::env::var_os(PROBE_ENV).map(PathBuf::from) else {
        return;
    };
    let repo = test_repo();
    let (up, rev1) = upstream(&root);
    let url = format!("{FAKE_HOST}up");
    let (edited, edited_dest) = install(&repo.store, &url, &rev1, "edited");
    let (plain, plain_dest) = install(&repo.store, &url, &rev1, "plain");
    write(&edited_dest, "SKILL.md", "---\nname: demo\n---\nMINE\n");
    let rev2 = commit(&up, "notes.md", "notes v2\n", "v2");
    let before = snapshot(&edited_dest);

    // The auto-updater with apply on skips the edited skill and updates the
    // other one.
    repo.store.set_setting("auto_update_apply", "on").unwrap();
    let round = skill_auto_updater::run_round_blocking(&repo.store).unwrap();
    assert_eq!(round.updated, 1);
    assert_eq!(round.held_back, 1);
    assert_eq!(snapshot(&edited_dest), before);
    assert_eq!(fs::read_to_string(plain_dest.join("notes.md")).unwrap(), "notes v2\n");
    let row = repo.store.get_skill_by_id(&edited).unwrap().unwrap();
    assert_eq!(row.update_status, "update_available");
    assert_eq!(row.source_revision.as_deref(), Some(rev1.as_str()));
    let row = repo.store.get_skill_by_id(&plain).unwrap().unwrap();
    assert_eq!(row.source_revision.as_deref(), Some(rev2.as_str()));

    // A pending library sync conflict holds even an unedited skill, as suspect,
    // for a manual update and for the auto-updater.
    let rev3 = commit(&up, "SKILL.md", "---\nname: demo\n---\nv3\n", "v3");
    repo.store
        .replace_pending_conflicts(&[PendingConflictRow {
            skill_id: plain.clone(),
            theirs_commit: "2".repeat(40),
            theirs_path: None,
            detected_at: 1,
        }])
        .unwrap();
    let before_plain = snapshot(&plain_dest);
    let suspect = cmd::update_git_skill_internal(&repo.store, &plain, None, None, None).unwrap();
    assert_eq!(suspect.overwrite_reason, Some(OverwriteReason::BaselineSuspect));
    assert!(suspect.pending_overwrites.iter().any(|o| o.path == "SKILL.md"));
    let round = skill_auto_updater::run_round_blocking(&repo.store).unwrap();
    assert_eq!(round.updated, 0);
    assert_eq!(round.held_back, 2);
    assert_eq!(snapshot(&plain_dest), before_plain);
    repo.store.replace_pending_conflicts(&[]).unwrap();

    // No baseline, revision still upstream: "not yet backfilled".
    baseline::forget(&plain).unwrap();
    let unknown = cmd::update_git_skill_internal(&repo.store, &plain, None, None, None).unwrap();
    assert_eq!(unknown.overwrite_reason, Some(OverwriteReason::BaselineNotBackfilled));
    assert_eq!(snapshot(&plain_dest), before_plain);

    // No baseline, revision gone upstream: "unrecoverable".
    repo.store
        .update_skill_source_metadata(
            &plain,
            None,
            None,
            None,
            Some("0123456789abcdef0123456789abcdef01234567"),
        )
        .unwrap();
    let gone = cmd::update_git_skill_internal(&repo.store, &plain, None, None, None).unwrap();
    assert_eq!(gone.overwrite_reason, Some(OverwriteReason::BaselineUnrecoverable));
    assert_eq!(snapshot(&plain_dest), before_plain);

    // Approving takes upstream once and records a baseline again.
    let taken = cmd::update_git_skill_internal(
        &repo.store,
        &plain,
        None,
        None,
        gone.removal_approval.as_deref(),
    )
    .unwrap();
    assert!(taken.content_changed);
    assert_eq!(baseline::load(&plain).unwrap().unwrap().revision, rev3);
    no_staged_leftovers();
}
