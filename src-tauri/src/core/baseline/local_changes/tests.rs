//! Local-change detection (sm-69q.3).

use super::*;
use crate::core::baseline::{forget, record, Origin};
use crate::core::central_repo;
use crate::core::skill_store::PendingConflictRow;
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

fn write_files(dir: &Path, files: &[(&str, &str)]) {
    for (rel, body) in files {
        let path = dir.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, body).unwrap();
    }
}

const REV1: &str = "1111111111111111111111111111111111111111";

fn skill_record(id: &str, dir: &Path, source_type: &str) -> SkillRecord {
    let now = chrono::Utc::now().timestamp_millis();
    SkillRecord {
        id: id.to_string(),
        name: id.to_string(),
        description: None,
        source_type: source_type.to_string(),
        source_ref: Some("https://example.invalid/repo.git".to_string()),
        source_ref_resolved: Some("https://example.invalid/repo.git".to_string()),
        source_subpath: None,
        source_branch: None,
        source_revision: Some(REV1.to_string()),
        remote_revision: Some(REV1.to_string()),
        central_path: dir.to_string_lossy().to_string(),
        content_hash: Some(content_hash::hash_directory(dir).unwrap()),
        enabled: true,
        created_at: now,
        updated_at: now,
        status: "ok".to_string(),
        update_status: "up_to_date".to_string(),
        last_checked_at: None,
        last_check_error: None,
    }
}

/// A git skill `id` in the library holding `files`, with a baseline recorded
/// from exactly those files.
fn baselined_skill(repo: &TestRepo, id: &str, files: &[(&str, &str)]) -> SkillRecord {
    let dir = central_repo::skills_dir().join(id);
    write_files(&dir, files);
    let skill = skill_record(id, &dir, "git");
    repo.store.insert_skill(&skill).unwrap();
    record(id, REV1, Origin::Install, &dir).unwrap();
    skill
}

fn changed(path: &str, state: FileChangeState) -> LocalFileChange {
    LocalFileChange {
        path: path.to_string(),
        state,
    }
}

#[test]
fn modified_added_and_deleted_files_are_reported_with_their_state() {
    let repo = test_repo();
    let skill = baselined_skill(
        &repo,
        "demo",
        &[
            ("SKILL.md", "---\nname: demo\n---\nv1\n"),
            ("refs/a.md", "a\n"),
            ("refs/gone.md", "gone\n"),
        ],
    );
    let clean = local_changes(&repo.store, &skill).unwrap();
    assert_eq!(clean.state, LocalState::Clean);
    assert_eq!(clean.change_count(), Some(0));
    assert_eq!(clean.baseline_revision.as_deref(), Some(REV1));

    let dir = Path::new(&skill.central_path);
    fs::write(dir.join("SKILL.md"), "---\nname: demo\n---\nmine\n").unwrap();
    fs::write(dir.join("refs/new.md"), "new\n").unwrap();
    fs::remove_file(dir.join("refs/gone.md")).unwrap();

    let found = local_changes(&repo.store, &skill).unwrap();
    assert_eq!(found.state, LocalState::Changed);
    assert!(found.baseline_known());
    assert_eq!(
        found.files,
        vec![
            changed("SKILL.md", FileChangeState::Modified),
            changed("refs/gone.md", FileChangeState::Deleted),
            changed("refs/new.md", FileChangeState::Added),
        ]
    );
    assert_eq!(found.change_count(), Some(3));
}

#[test]
fn an_edit_on_disk_shows_at_the_next_call_and_reverting_clears_it() {
    let repo = test_repo();
    let body = "---\nname: demo\n---\nv1\n";
    let skill = baselined_skill(&repo, "demo", &[("SKILL.md", body)]);
    // Prime the cache.
    assert_eq!(
        local_changes(&repo.store, &skill).unwrap().state,
        LocalState::Clean
    );
    assert_eq!(
        local_changes(&repo.store, &skill).unwrap().state,
        LocalState::Clean
    );

    let file = Path::new(&skill.central_path).join("SKILL.md");
    fs::write(&file, "---\nname: demo\n---\nv1, edited\n").unwrap();
    let edited = local_changes(&repo.store, &skill).unwrap();
    assert_eq!(edited.state, LocalState::Changed);
    assert_eq!(
        edited.files,
        vec![changed("SKILL.md", FileChangeState::Modified)]
    );

    fs::write(&file, body).unwrap();
    assert_eq!(
        local_changes(&repo.store, &skill).unwrap().state,
        LocalState::Clean
    );
}

#[test]
fn a_change_that_only_flips_line_endings_is_clean() {
    let repo = test_repo();
    let skill = baselined_skill(
        &repo,
        "demo",
        &[("SKILL.md", "---\nname: demo\n---\nline one\nline two\n")],
    );
    fs::write(
        Path::new(&skill.central_path).join("SKILL.md"),
        "---\r\nname: demo\r\n---\r\nline one\r\nline two\r\n",
    )
    .unwrap();
    let found = local_changes(&repo.store, &skill).unwrap();
    assert_eq!(found.state, LocalState::Clean, "{found:?}");
    assert!(found.files.is_empty());
}

#[test]
fn a_crlf_baseline_read_against_lf_content_is_clean_although_the_hash_differs() {
    let repo = test_repo();
    // Recorded on a device whose checkout had CRLF endings.
    let staged = repo.tmp.path().join("staged-crlf");
    write_files(
        &staged,
        &[
            ("SKILL.md", "---\r\nname: demo\r\n---\r\nbody\r\n"),
            ("refs/a.md", "a\r\nb\r\n"),
        ],
    );
    let dir = central_repo::skills_dir().join("demo");
    write_files(
        &dir,
        &[
            ("SKILL.md", "---\nname: demo\n---\nbody\n"),
            ("refs/a.md", "a\nb\n"),
        ],
    );
    let skill = skill_record("demo", &dir, "git");
    repo.store.insert_skill(&skill).unwrap();
    let baseline = record("demo", REV1, Origin::Install, &staged).unwrap();
    assert_ne!(
        baseline.content_hash.as_deref(),
        Some(content_hash::hash_directory(&dir).unwrap().as_str()),
        "the raw-byte hint must differ for this test to mean anything"
    );

    let found = local_changes(&repo.store, &skill).unwrap();
    assert_eq!(found.state, LocalState::Clean, "{found:?}");
}

#[test]
fn a_git_skill_with_no_baseline_is_unknown_not_clean() {
    let repo = test_repo();
    let dir = central_repo::skills_dir().join("demo");
    write_files(&dir, &[("SKILL.md", "---\nname: demo\n---\n")]);
    let skill = skill_record("demo", &dir, "git");
    repo.store.insert_skill(&skill).unwrap();

    let found = local_changes(&repo.store, &skill).unwrap();
    assert_eq!(found.state, LocalState::BaselineUnknown);
    assert!(!found.baseline_known());
    assert_eq!(found.change_count(), None);

    // Same after a recorded baseline is forgotten.
    let skill = baselined_skill(&repo, "other", &[("SKILL.md", "x\n")]);
    forget("other").unwrap();
    assert_eq!(
        local_changes(&repo.store, &skill).unwrap().state,
        LocalState::BaselineUnknown
    );
}

#[test]
fn a_skill_installed_from_a_local_folder_has_no_baseline_and_no_marker() {
    let repo = test_repo();
    let dir = central_repo::skills_dir().join("mine");
    write_files(&dir, &[("SKILL.md", "---\nname: mine\n---\n")]);
    let skill = skill_record("mine", &dir, "local");
    repo.store.insert_skill(&skill).unwrap();

    let found = local_changes(&repo.store, &skill).unwrap();
    assert_eq!(found.state, LocalState::Untracked);
    assert_eq!(found.baseline_revision, None);
    assert!(!found.baseline_known());
    assert!(!found.is_changed());
    assert!(found.files.is_empty());
}

#[test]
fn a_pending_sync_conflict_makes_the_baseline_suspect_until_resolved() {
    let repo = test_repo();
    let skill = baselined_skill(&repo, "demo", &[("SKILL.md", "v1\n")]);
    fs::write(Path::new(&skill.central_path).join("SKILL.md"), "mine\n").unwrap();
    assert_eq!(
        local_changes(&repo.store, &skill).unwrap().state,
        LocalState::Changed
    );

    repo.store
        .replace_pending_conflicts(&[PendingConflictRow {
            skill_id: "demo".to_string(),
            theirs_commit: "2222222222222222222222222222222222222222".to_string(),
            theirs_path: None,
            detected_at: 1,
        }])
        .unwrap();
    let suspect = local_changes(&repo.store, &skill).unwrap();
    assert_eq!(suspect.state, LocalState::BaselineSuspect);
    assert!(suspect.files.is_empty());
    assert!(!suspect.baseline_known());
    // Distinct from unknown, but it still names the baseline it doubts.
    assert_eq!(suspect.baseline_revision.as_deref(), Some(REV1));

    repo.store.replace_pending_conflicts(&[]).unwrap();
    assert_eq!(
        local_changes(&repo.store, &skill).unwrap().state,
        LocalState::Changed
    );
}

#[test]
fn local_changes_for_reads_many_skills() {
    let repo = test_repo();
    let a = baselined_skill(&repo, "a", &[("SKILL.md", "a\n")]);
    let b = baselined_skill(&repo, "b", &[("SKILL.md", "b\n")]);
    fs::write(Path::new(&b.central_path).join("SKILL.md"), "b2\n").unwrap();
    let map = local_changes_for(&repo.store, &[a, b]).unwrap();
    assert_eq!(map["a"].state, LocalState::Clean);
    assert_eq!(map["b"].state, LocalState::Changed);
}

#[test]
fn local_diff_uses_the_base_store_and_skips_eol_only_files() {
    let repo = test_repo();
    let skill = baselined_skill(
        &repo,
        "demo",
        &[
            ("SKILL.md", "---\nname: demo\n---\nv1\n"),
            ("eol.md", "one\ntwo\n"),
        ],
    );
    let dir = Path::new(&skill.central_path);
    fs::write(dir.join("SKILL.md"), "---\r\nname: demo\r\n---\r\nmine\r\n").unwrap();
    fs::write(dir.join("eol.md"), "one\r\ntwo\r\n").unwrap();

    let diff = local_diff(&repo.store, &skill, None).unwrap();
    assert_eq!(diff.changes.state, LocalState::Changed);
    assert_eq!(diff.base_source, Some(BaseSource::BaseStore));
    let entries = diff.entries.expect("a diff");
    assert_eq!(entries.len(), 1, "only the real change: {entries:?}");
    assert_eq!(entries[0].relative_path, "SKILL.md");
    assert_eq!(entries[0].status, "modified");
    assert_eq!(
        entries[0].original_text.as_deref(),
        Some("---\nname: demo\n---\nv1\n")
    );
    assert_eq!(
        entries[0].updated_text.as_deref(),
        Some("---\nname: demo\n---\nmine\n")
    );
}

#[test]
fn local_diff_without_base_content_lists_the_changed_paths() {
    let repo = test_repo();
    let skill = baselined_skill(&repo, "demo", &[("SKILL.md", "v1\n")]);
    fs::write(Path::new(&skill.central_path).join("SKILL.md"), "mine\n").unwrap();
    // No base store on this device, and the upstream cannot be reached.
    fs::remove_dir_all(base_store_path()).unwrap();

    let diff = local_diff_inner(&repo.store, &skill, None, false).unwrap();
    assert_eq!(diff.changes.state, LocalState::Changed);
    assert_eq!(
        diff.changes.files,
        vec![changed("SKILL.md", FileChangeState::Modified)]
    );
    assert!(diff.entries.is_none());
    assert!(diff.message.unwrap().contains("baseline content unavailable"));
}

#[test]
fn local_diff_reports_why_there_is_no_diff() {
    let repo = test_repo();
    let dir = central_repo::skills_dir().join("demo");
    write_files(&dir, &[("SKILL.md", "x\n")]);
    let skill = skill_record("demo", &dir, "git");
    repo.store.insert_skill(&skill).unwrap();
    let diff = local_diff(&repo.store, &skill, None).unwrap();
    assert_eq!(diff.changes.state, LocalState::BaselineUnknown);
    assert!(diff.entries.is_none());
    assert!(diff.message.unwrap().contains("baseline unknown"));

    let clean = baselined_skill(&repo, "clean", &[("SKILL.md", "c\n")]);
    let diff = local_diff(&repo.store, &clean, None).unwrap();
    assert_eq!(diff.entries.map(|e| e.len()), Some(0));
}

// ── fetching the baseline revision ──

fn git_in(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.email=t@t", "-c", "user.name=t"])
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn file_url(path: &Path) -> String {
    let raw = path.display().to_string().replace('\\', "/");
    if raw.starts_with('/') {
        format!("file://{raw}")
    } else {
        format!("file:///{raw}")
    }
}

#[test]
fn local_diff_fetches_the_baseline_revision_when_the_base_store_lacks_it() {
    let repo = test_repo();
    let up = repo.tmp.path().join("upstream");
    write_files(
        &up,
        &[
            ("skills/demo/SKILL.md", "---\nname: demo\n---\nv1\n"),
            ("skills/demo/refs/a.md", "a\n"),
        ],
    );
    git_in(&up, &["init", "-q", "-b", "main"]);
    git_in(&up, &["add", "-A"]);
    git_in(&up, &["commit", "-qm", "v1"]);
    let rev1 = git_in(&up, &["rev-parse", "HEAD"]);
    // Upstream moves on; the baseline stays at rev1.
    fs::write(up.join("skills/demo/SKILL.md"), "---\nname: demo\n---\nv2\n").unwrap();
    git_in(&up, &["commit", "-qam", "v2"]);

    let dir = central_repo::skills_dir().join("demo");
    write_files(
        &dir,
        &[
            ("SKILL.md", "---\nname: demo\n---\nv1\n"),
            ("refs/a.md", "a, edited\n"),
        ],
    );
    let url = file_url(&up);
    let skill = SkillRecord {
        source_ref: Some(url.clone()),
        source_ref_resolved: Some(url),
        source_subpath: Some("skills/demo".to_string()),
        source_revision: Some(rev1.clone()),
        ..skill_record("demo", &dir, "git")
    };
    repo.store.insert_skill(&skill).unwrap();
    let staged = repo.tmp.path().join("staged");
    write_files(
        &staged,
        &[
            ("SKILL.md", "---\nname: demo\n---\nv1\n"),
            ("refs/a.md", "a\n"),
        ],
    );
    record("demo", &rev1, Origin::Install, &staged).unwrap();
    fs::remove_dir_all(base_store_path()).unwrap();

    let diff = local_diff_inner(&repo.store, &skill, None, false).unwrap();
    assert_eq!(diff.base_source, Some(BaseSource::Fetch), "{diff:?}");
    let entries = diff.entries.expect("a diff");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].relative_path, "refs/a.md");
    assert_eq!(entries[0].original_text.as_deref(), Some("a\n"));
    assert_eq!(entries[0].updated_text.as_deref(), Some("a, edited\n"));
}
