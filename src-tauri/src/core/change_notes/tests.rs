//! Change notes and patch export (sm-69q.8).

use super::*;
use crate::core::baseline::{record, Origin};
use crate::core::skill_store::PendingConflictRow;
use crate::core::{central_repo, content_hash, sync_metadata};
use std::fs;
use std::process::Command;
use std::sync::MutexGuard;
use tempfile::{tempdir, TempDir};

struct TestRepo {
    _lock: MutexGuard<'static, ()>,
    tmp: TempDir,
    db: PathBuf,
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
    let db = base.join("test.db");
    let store = SkillStore::new(&db).unwrap();
    TestRepo {
        _lock: lock,
        tmp,
        db,
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
const REV2: &str = "2222222222222222222222222222222222222222";

fn skill_record(id: &str, dir: &Path, url: &str, subpath: Option<&str>) -> SkillRecord {
    let now = chrono::Utc::now().timestamp_millis();
    SkillRecord {
        id: id.to_string(),
        name: id.to_string(),
        description: None,
        source_type: "git".to_string(),
        source_ref: Some(url.to_string()),
        source_ref_resolved: Some(url.to_string()),
        source_subpath: subpath.map(str::to_string),
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

/// A git skill `id` in the library holding `files`, with a baseline at
/// REV1 recorded from exactly those files (so the base store has them).
fn baselined_skill(repo: &TestRepo, id: &str, files: &[(&str, &str)]) -> SkillRecord {
    let dir = central_repo::skills_dir().join(id);
    write_files(&dir, files);
    let skill = skill_record(id, &dir, "https://example.invalid/repo.git", None);
    repo.store.insert_skill(&skill).unwrap();
    record(id, REV1, Origin::Install, &dir).unwrap();
    skill
}

fn note_of(report: &ChangesReport, path: &str) -> Option<String> {
    report
        .files
        .iter()
        .find(|f| f.path == path)
        .and_then(|f| f.note.clone())
}

fn base_store() -> PathBuf {
    central_repo::cache_dir().join("baselines.git")
}

// ── notes ──

#[test]
fn a_note_is_listed_with_its_file_and_survives_a_restart() {
    let repo = test_repo();
    let skill = baselined_skill(
        &repo,
        "demo",
        &[
            ("SKILL.md", "---\nname: demo\n---\nv1\n"),
            ("refs/a.md", "a\n"),
        ],
    );
    let dir = Path::new(&skill.central_path);
    fs::write(dir.join("refs/a.md"), "a, Windows paths\n").unwrap();
    fs::write(dir.join("refs/new.md"), "new\n").unwrap();

    let report = set_note(&repo.store, &skill, "refs\\a.md", "  Windows\n paths  ").unwrap();
    assert_eq!(report.state, LocalState::Changed);
    assert_eq!(
        note_of(&report, "refs/a.md").as_deref(),
        Some("Windows paths")
    );
    assert_eq!(note_of(&report, "refs/new.md"), None);
    assert_eq!(report.files.len(), 2);
    assert!(report.files[0].note_updated_at.is_some());

    // A restart: a fresh store over the same database, and the notes read
    // back from the synced file.
    let reopened = SkillStore::new(&repo.db).unwrap();
    let skill = reopened.get_skill_by_id("demo").unwrap().unwrap();
    let report = changes_report(&reopened, &skill).unwrap();
    assert_eq!(
        note_of(&report, "refs/a.md").as_deref(),
        Some("Windows paths")
    );
    assert_eq!(
        fork_files::path_for(ForkConcern::Notes, "demo")
            .unwrap()
            .parent()
            .unwrap(),
        sync_metadata::metadata_dir()
    );
}

#[test]
fn a_note_survives_a_library_sync_reindex_and_rewrite() {
    let repo = test_repo();
    let skill = baselined_skill(&repo, "demo", &[("SKILL.md", "v1\n")]);
    fs::write(Path::new(&skill.central_path).join("SKILL.md"), "mine\n").unwrap();
    set_note(&repo.store, &skill, "SKILL.md", "removed the usage ping").unwrap();

    sync_metadata::write_all_from_db(&repo.store).unwrap();
    sync_metadata::reindex_from_metadata(&repo.store).unwrap();

    let skill = repo.store.get_skill_by_id("demo").unwrap().unwrap();
    let report = changes_report(&repo.store, &skill).unwrap();
    assert_eq!(
        note_of(&report, "SKILL.md").as_deref(),
        Some("removed the usage ping")
    );
}

#[test]
fn reverting_a_file_drops_its_note_and_only_its_note() {
    let repo = test_repo();
    let skill = baselined_skill(&repo, "demo", &[("SKILL.md", "v1\n"), ("b.md", "b\n")]);
    let dir = Path::new(&skill.central_path);
    fs::write(dir.join("SKILL.md"), "mine\n").unwrap();
    fs::write(dir.join("b.md"), "b, mine\n").unwrap();
    set_note(&repo.store, &skill, "SKILL.md", "reason one").unwrap();
    set_note(&repo.store, &skill, "b.md", "reason two").unwrap();

    fs::write(dir.join("SKILL.md"), "v1\n").unwrap();
    let report = changes_report(&repo.store, &skill).unwrap();
    assert_eq!(report.files.len(), 1);
    assert_eq!(note_of(&report, "b.md").as_deref(), Some("reason two"));
    let stored = load("demo").unwrap();
    assert_eq!(stored.len(), 1, "{stored:?}");
    assert_eq!(stored[0].path, "b.md");

    // Editing it again does not bring the old note back.
    fs::write(dir.join("SKILL.md"), "mine again\n").unwrap();
    let report = changes_report(&repo.store, &skill).unwrap();
    assert_eq!(note_of(&report, "SKILL.md"), None);

    // Reverting the last one removes the file.
    fs::write(dir.join("SKILL.md"), "v1\n").unwrap();
    fs::write(dir.join("b.md"), "b\n").unwrap();
    let report = changes_report(&repo.store, &skill).unwrap();
    assert_eq!(report.state, LocalState::Clean);
    assert!(!fork_files::path_for(ForkConcern::Notes, "demo")
        .unwrap()
        .exists());
}

#[test]
fn a_note_survives_a_clean_merge_of_upstream_changes_to_other_files() {
    let repo = test_repo();
    let skill = baselined_skill(
        &repo,
        "demo",
        &[
            ("SKILL.md", "v1\n"),
            ("refs/a.md", "a\n"),
            ("refs/c.md", "c\n"),
        ],
    );
    let dir = Path::new(&skill.central_path);
    fs::write(dir.join("refs/a.md"), "a, mine\n").unwrap();
    fs::write(dir.join("refs/c.md"), "c, mine\n").unwrap();
    set_note(&repo.store, &skill, "refs/a.md", "kept across the merge").unwrap();
    set_note(&repo.store, &skill, "refs/c.md", "upstream took this").unwrap();

    // Upstream REV2 changed SKILL.md and took the same edit to c.md. A clean
    // merge brings both in and keeps a.md; the new baseline is REV2.
    let upstream = repo.tmp.path().join("rev2");
    write_files(
        &upstream,
        &[
            ("SKILL.md", "v2\n"),
            ("refs/a.md", "a\n"),
            ("refs/c.md", "c, mine\n"),
        ],
    );
    fs::write(dir.join("SKILL.md"), "v2\n").unwrap();
    record("demo", REV2, Origin::Merge, &upstream).unwrap();

    let report = changes_report(&repo.store, &skill).unwrap();
    assert_eq!(report.baseline_revision.as_deref(), Some(REV2));
    assert_eq!(report.files.len(), 1, "{:?}", report.files);
    assert_eq!(
        note_of(&report, "refs/a.md").as_deref(),
        Some("kept across the merge")
    );
    assert!(load("demo").unwrap().iter().all(|n| n.path != "refs/c.md"));
}

#[test]
fn notes_are_held_not_dropped_while_the_baseline_is_suspect() {
    let repo = test_repo();
    let skill = baselined_skill(&repo, "demo", &[("SKILL.md", "v1\n")]);
    fs::write(Path::new(&skill.central_path).join("SKILL.md"), "mine\n").unwrap();
    set_note(&repo.store, &skill, "SKILL.md", "why").unwrap();
    repo.store
        .replace_pending_conflicts(&[PendingConflictRow {
            skill_id: "demo".to_string(),
            theirs_commit: REV2.to_string(),
            theirs_path: None,
            detected_at: 1,
        }])
        .unwrap();

    let report = changes_report(&repo.store, &skill).unwrap();
    assert_eq!(report.state, LocalState::BaselineSuspect);
    assert!(report.files.is_empty());
    assert_eq!(report.held_notes.len(), 1);
    assert_eq!(load("demo").unwrap().len(), 1);
    let err = set_note(&repo.store, &skill, "SKILL.md", "x").unwrap_err();
    assert!(format!("{err:#}").contains("suspect"), "{err:#}");
}

#[test]
fn a_note_needs_a_changed_file_and_clear_removes_it() {
    let repo = test_repo();
    let skill = baselined_skill(&repo, "demo", &[("SKILL.md", "v1\n"), ("b.md", "b\n")]);
    fs::write(Path::new(&skill.central_path).join("SKILL.md"), "mine\n").unwrap();

    let err = set_note(&repo.store, &skill, "b.md", "nope").unwrap_err();
    assert!(format!("{err:#}").contains("does not differ"), "{err:#}");
    let err = set_note(
        &repo.store,
        &skill,
        "SKILL.md",
        &"x".repeat(MAX_NOTE_CHARS + 1),
    )
    .unwrap_err();
    assert!(format!("{err:#}").contains("at most"), "{err:#}");

    set_note(&repo.store, &skill, "./SKILL.md", "first").unwrap();
    let report = set_note(&repo.store, &skill, "SKILL.md", "second").unwrap();
    assert_eq!(note_of(&report, "SKILL.md").as_deref(), Some("second"));
    assert_eq!(load("demo").unwrap().len(), 1);

    let report = clear_note(&repo.store, &skill, "SKILL.md").unwrap();
    assert_eq!(note_of(&report, "SKILL.md"), None);
    assert!(load("demo").unwrap().is_empty());
    // An empty note clears too, and clearing twice is fine.
    set_note(&repo.store, &skill, "SKILL.md", "again").unwrap();
    set_note(&repo.store, &skill, "SKILL.md", "   ").unwrap();
    clear_note(&repo.store, &skill, "SKILL.md").unwrap();
    assert!(load("demo").unwrap().is_empty());
}

#[test]
fn a_skill_without_a_baseline_takes_no_note() {
    let repo = test_repo();
    let dir = central_repo::skills_dir().join("demo");
    write_files(&dir, &[("SKILL.md", "x\n")]);
    let skill = skill_record("demo", &dir, "https://example.invalid/repo.git", None);
    repo.store.insert_skill(&skill).unwrap();
    let err = set_note(&repo.store, &skill, "SKILL.md", "why").unwrap_err();
    assert!(format!("{err:#}").contains("baseline unknown"), "{err:#}");
    let err = export_patch(&repo.store, &skill, None).unwrap_err();
    assert!(
        format!("{err:#}").contains("baseline is unknown"),
        "{err:#}"
    );
}

// ── patch export ──

fn git_in(dir: &Path, args: &[&str]) -> std::process::Output {
    Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "-c",
            "core.autocrlf=false",
        ])
        .args(args)
        .output()
        .unwrap()
}

fn git_ok(dir: &Path, args: &[&str]) -> String {
    let out = git_in(dir, args);
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

const UPSTREAM_V1: &[(&str, &str)] = &[
    ("README.md", "repo readme\n"),
    (
        "skills/demo/SKILL.md",
        "---\nname: demo\n---\nline one\nline two\nline three\n",
    ),
    ("skills/demo/refs/a.md", "a1\na2\na3\n"),
    ("skills/demo/refs/gone.md", "gone\n"),
    ("skills/demo/refs/eol.md", "one\ntwo\n"),
];

/// An upstream repository with the skill in `skills/demo` at rev1, moved on
/// by a later commit; the library copy installed from rev1 and then edited
/// (with CRLF endings, as a Windows editor writes them), the baseline at
/// rev1 in the base store, and notes on two files.
struct Fixture {
    up: PathBuf,
    rev1: String,
    skill: SkillRecord,
}

fn fixture(repo: &TestRepo) -> Fixture {
    let up = repo.tmp.path().join("upstream");
    write_files(&up, UPSTREAM_V1);
    git_ok(&up, &["init", "-q", "-b", "main"]);
    git_ok(&up, &["add", "-A"]);
    git_ok(&up, &["commit", "-qm", "v1"]);
    let rev1 = git_ok(&up, &["rev-parse", "HEAD"]);
    fs::write(up.join("skills/demo/refs/a.md"), "a1\nupstream a2\na3\n").unwrap();
    git_ok(&up, &["commit", "-qam", "v2"]);

    let staged = repo.tmp.path().join("staged");
    for (rel, body) in UPSTREAM_V1 {
        if let Some(rel) = rel.strip_prefix("skills/demo/") {
            write_files(&staged, &[(rel, body)]);
        }
    }
    let dir = central_repo::skills_dir().join("demo");
    write_files(
        &dir,
        &[
            (
                "SKILL.md",
                "---\r\nname: demo\r\n---\r\nline one\r\nline two, mine\r\nline three\r\n",
            ),
            ("refs/a.md", "a1\na2\na3\n"),
            ("refs/eol.md", "one\r\ntwo\r\n"),
            ("refs/new.md", "brand new\n"),
        ],
    );
    let url = file_url(&up);
    let skill = SkillRecord {
        source_revision: Some(rev1.clone()),
        ..skill_record("demo", &dir, &url, Some("skills/demo/"))
    };
    repo.store.insert_skill(&skill).unwrap();
    record("demo", &rev1, Origin::Install, &staged).unwrap();
    set_note(&repo.store, &skill, "SKILL.md", "line two says what we do").unwrap();
    set_note(&repo.store, &skill, "refs/gone.md", "not needed here").unwrap();
    Fixture { up, rev1, skill }
}

/// A fresh checkout of the baseline revision.
fn checkout_of(repo: &TestRepo, fx: &Fixture, name: &str) -> PathBuf {
    let co = repo.tmp.path().join(name);
    let out = Command::new("git")
        .args(["-c", "core.autocrlf=false", "clone", "-q"])
        .arg(&fx.up)
        .arg(&co)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    git_ok(&co, &["checkout", "-q", &fx.rev1]);
    co
}

fn assert_applies(repo: &TestRepo, fx: &Fixture, export: &PatchExport, name: &str) {
    let co = checkout_of(repo, fx, name);
    let patch_file = repo.tmp.path().join(format!("{name}.patch"));
    fs::write(&patch_file, &export.patch).unwrap();
    let patch_arg = patch_file.to_string_lossy().to_string();
    git_ok(&co, &["apply", "--check", &patch_arg]);
    git_ok(&co, &["apply", &patch_arg]);

    let skill_dir = co.join("skills/demo");
    let read = |rel: &str| fs::read_to_string(skill_dir.join(rel)).unwrap();
    assert_eq!(
        read("SKILL.md"),
        "---\nname: demo\n---\nline one\nline two, mine\nline three\n"
    );
    assert_eq!(read("refs/new.md"), "brand new\n");
    assert_eq!(read("refs/a.md"), "a1\na2\na3\n");
    assert_eq!(read("refs/eol.md"), "one\ntwo\n");
    assert!(!skill_dir.join("refs/gone.md").exists());
    assert_eq!(read("../../README.md"), "repo readme\n");
}

#[test]
fn the_exported_patch_applies_to_a_fresh_checkout_of_the_baseline_revision() {
    let repo = test_repo();
    let fx = fixture(&repo);

    let export = export_patch_inner(&repo.store, &fx.skill, None, false).unwrap();
    assert_eq!(export.base_source, BaseSource::BaseStore);
    assert_eq!(export.path_prefix, "skills/demo");
    assert_eq!(export.baseline_revision, fx.rev1);
    assert_eq!(export.file_count, 3, "{}", export.text());
    assert_eq!(export.noted_file_count, 2);

    let text = export.text();
    assert!(
        text.starts_with("# skills-manager: local changes to demo (demo)\n"),
        "{text}"
    );
    assert!(text.contains(&format!("# Baseline revision: {}\n", fx.rev1)));
    // Each note sits directly above its file's header.
    assert!(
        text.contains(
            "# SKILL.md: line two says what we do\ndiff --git a/skills/demo/SKILL.md b/skills/demo/SKILL.md\n"
        ),
        "{text}"
    );
    assert!(
        text.contains("# refs/gone.md: not needed here\ndiff --git a/skills/demo/refs/gone.md"),
        "{text}"
    );
    assert!(text.contains("new file mode 100644"), "{text}");
    assert!(text.contains("deleted file mode 100644"), "{text}");
    // Line endings are no change: eol.md is not in it, nor are CRLFs.
    assert!(!text.contains("eol.md"), "{text}");
    assert!(!text.contains('\r'), "{text:?}");

    assert_applies(&repo, &fx, &export, "checkout-store");
}

#[test]
fn the_patch_fetches_the_baseline_revision_when_the_base_store_lacks_it() {
    let repo = test_repo();
    let fx = fixture(&repo);
    fs::remove_dir_all(base_store()).unwrap();

    let export = export_patch_inner(&repo.store, &fx.skill, None, false).unwrap();
    assert_eq!(export.base_source, BaseSource::Fetch);
    assert_applies(&repo, &fx, &export, "checkout-fetch");
}

#[test]
fn export_fails_clearly_when_the_baseline_content_cannot_be_had() {
    let repo = test_repo();
    let fx = fixture(&repo);
    fs::remove_dir_all(base_store()).unwrap();
    fs::remove_dir_all(&fx.up).unwrap();

    let err = export_patch_inner(&repo.store, &fx.skill, None, false).unwrap_err();
    let msg = format!("{err:#}");
    assert!(msg.contains("baseline content"), "{msg}");
    assert!(msg.contains(&fx.rev1), "{msg}");
}

#[test]
fn export_of_a_clean_skill_says_there_is_nothing_to_export() {
    let repo = test_repo();
    let skill = baselined_skill(&repo, "demo", &[("SKILL.md", "v1\n")]);
    let err = export_patch(&repo.store, &skill, None).unwrap_err();
    assert!(format!("{err:#}").contains("nothing to export"), "{err:#}");
}

#[test]
fn a_crlf_baseline_gets_a_crlf_library_side() {
    assert_eq!(
        match_line_endings(b"a\r\nb\r\n", b"a\nc\n".to_vec()),
        b"a\r\nc\r\n".to_vec()
    );
    assert_eq!(
        match_line_endings(b"a\nb\n", b"a\r\nc\r\n".to_vec()),
        b"a\nc\n".to_vec()
    );
    // Mixed or binary: untouched.
    assert_eq!(
        match_line_endings(b"a\r\nb\n", b"a\r\nc\n".to_vec()),
        b"a\r\nc\n".to_vec()
    );
    assert_eq!(
        match_line_endings(b"\0a\n", b"\0a\r\n".to_vec()),
        b"\0a\r\n".to_vec()
    );
}
