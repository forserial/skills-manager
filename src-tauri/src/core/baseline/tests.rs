//! Baseline recording, storage and backfill (sm-69q.2).
//!
//! The update and set-source commands refuse `file://` URLs, so the
//! end-to-end test runs in a child test process whose git is told, through
//! `GIT_CONFIG_*` in the child's environment only, to fetch a made-up https
//! host from a local repository. The parent's environment is never touched.

use super::*;
use crate::commands::skills as cmd;
use crate::core::skill_store::SkillRecord;
use crate::core::sync_metadata;
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

fn skill_record(id: &str, dir: &Path, source_type: &str, revision: Option<&str>) -> SkillRecord {
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
        source_revision: revision.map(str::to_string),
        remote_revision: revision.map(str::to_string),
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

const REV1: &str = "1111111111111111111111111111111111111111";

#[test]
fn manifest_is_the_same_for_lf_and_crlf_copies() {
    let tmp = tempdir().unwrap();
    let lf = tmp.path().join("lf");
    let crlf = tmp.path().join("crlf");
    write_files(
        &lf,
        &[
            ("SKILL.md", "---\nname: x\n---\nbody\n"),
            ("refs/a.md", "one\ntwo\n"),
        ],
    );
    write_files(
        &crlf,
        &[
            ("SKILL.md", "---\r\nname: x\r\n---\r\nbody\r\n"),
            ("refs/a.md", "one\r\ntwo\r\n"),
        ],
    );
    // A binary with CRLF bytes is not folded: it must stay different.
    fs::write(lf.join("blob.bin"), [0u8, b'\n', 1]).unwrap();
    fs::write(crlf.join("blob.bin"), [0u8, b'\r', b'\n', 1]).unwrap();

    let a = manifest_of(&lf).unwrap();
    let b = manifest_of(&crlf).unwrap();
    assert_eq!(
        a.iter().map(|f| f.path.as_str()).collect::<Vec<_>>(),
        vec!["SKILL.md", "blob.bin", "refs/a.md"],
        "paths use '/' and are sorted"
    );
    assert_eq!(differing_paths(&a, &b), vec!["blob.bin"]);
}

#[test]
fn differing_paths_lists_changed_added_and_removed() {
    let f = |path: &str, sha: &str| BaselineFile {
        path: path.to_string(),
        sha256: sha.to_string(),
    };
    let base = vec![f("a", "1"), f("b", "2"), f("c", "3")];
    let now = vec![f("a", "1"), f("b", "X"), f("d", "4")];
    assert_eq!(differing_paths(&base, &now), vec!["b", "c", "d"]);
}

#[test]
fn record_writes_the_flat_file_and_the_base_store_and_forget_drops_both() {
    let repo = test_repo();
    let dir = central_repo::skills_dir().join("demo");
    write_files(
        &dir,
        &[("SKILL.md", "---\nname: demo\n---\n"), ("sub/x.txt", "x\n")],
    );
    repo.store
        .insert_skill(&skill_record("demo", &dir, "git", Some(REV1)))
        .unwrap();

    let recorded = record("demo", REV1, Origin::Install, &dir).unwrap();
    let path = sync_metadata::metadata_dir().join("fork-baseline.demo.json");
    assert!(path.is_file());
    assert_eq!(load("demo").unwrap(), Some(recorded.clone()));
    assert_eq!(recorded.revision, REV1);
    assert_eq!(recorded.files.len(), 2);
    assert_eq!(
        recorded.content_hash.as_deref(),
        Some(content_hash::hash_directory(&dir).unwrap().as_str())
    );
    let tree_id = recorded.tree_id.clone().expect("base store tree");
    assert!(base_store_has_tree(&tree_id));
    // The tree holds the raw bytes at their paths.
    let store = Repository::open_bare(base_store_path()).unwrap();
    let tree = store.find_tree(Oid::from_str(&tree_id).unwrap()).unwrap();
    let entry = tree.get_path(Path::new("sub/x.txt")).unwrap();
    assert_eq!(store.find_blob(entry.id()).unwrap().content(), b"x\n");
    assert!(store.find_reference("refs/baselines/demo").is_ok());

    let summary = summary_for("demo").unwrap();
    assert_eq!(summary.file_count, 2);
    assert_eq!(summary.origin, Origin::Install);

    forget("demo").unwrap();
    assert!(!path.exists());
    assert_eq!(load("demo").unwrap(), None);
    assert!(store.find_reference("refs/baselines/demo").is_err());
}

#[test]
fn restart_and_rewrite_leave_the_baseline_unchanged_after_a_library_edit() {
    let repo = test_repo();
    let dir = central_repo::skills_dir().join("demo");
    write_files(&dir, &[("SKILL.md", "---\nname: demo\n---\nupstream\n")]);
    repo.store
        .insert_skill(&skill_record("demo", &dir, "git", Some(REV1)))
        .unwrap();
    sync_metadata::write_all_from_db(&repo.store).unwrap();
    let recorded = record("demo", REV1, Origin::Install, &dir).unwrap();
    let raw = fs::read(fork_files::path_for(ForkConcern::Baseline, "demo").unwrap()).unwrap();

    // The user edits the library copy; then the app restarts.
    fs::write(dir.join("SKILL.md"), "---\nname: demo\n---\nlocal edit\n").unwrap();
    sync_metadata::reindex_from_metadata(&repo.store).unwrap();
    sync_metadata::write_all_from_db(&repo.store).unwrap();

    // The library hash followed the edit; the baseline did not.
    let row = repo.store.get_skill_by_id("demo").unwrap().unwrap();
    assert_ne!(row.content_hash, recorded.content_hash);
    assert_eq!(load("demo").unwrap(), Some(recorded.clone()));
    assert_eq!(
        fs::read(fork_files::path_for(ForkConcern::Baseline, "demo").unwrap()).unwrap(),
        raw
    );
    assert_eq!(
        differing_paths(&recorded.files, &manifest_of(&dir).unwrap()),
        vec!["SKILL.md"]
    );
}

#[test]
fn install_hook_records_git_installs_and_forgets_on_a_non_git_reinstall() {
    let repo = test_repo();
    let src = repo.tmp.path().join("src");
    write_files(&src, &[("SKILL.md", "---\nname: demo\n---\n")]);
    let dest = central_repo::skills_dir().join("demo");
    let result = installer::install_skill_dir_to_destination(&src, "demo", &dest).unwrap();
    let git_meta = cmd::InstallSourceMetadata {
        source_type: "git".to_string(),
        source_ref: Some("https://example.invalid/repo.git".to_string()),
        source_ref_resolved: Some("https://example.invalid/repo.git".to_string()),
        source_subpath: None,
        source_branch: None,
        source_revision: Some(REV1.to_string()),
        remote_revision: Some(REV1.to_string()),
        update_status: "up_to_date".to_string(),
    };
    let id = {
        let _lock = RepoLock::acquire_foreground("test").unwrap();
        cmd::store_installed_skill_unlocked(&repo.store, &result, &git_meta, None).unwrap()
    };
    let row = repo.store.get_skill_by_id(&id).unwrap().unwrap();
    let baseline = load(&id).unwrap().expect("install records a baseline");
    assert_eq!(
        Some(baseline.revision.as_str()),
        row.source_revision.as_deref()
    );
    assert_eq!(baseline.origin, Origin::Install);

    // Reinstalled from a local folder: the content is no longer upstream's.
    let local_meta = cmd::InstallSourceMetadata {
        source_type: "local".to_string(),
        source_ref: Some(src.to_string_lossy().to_string()),
        source_ref_resolved: None,
        source_subpath: None,
        source_branch: None,
        source_revision: None,
        remote_revision: None,
        update_status: "local_only".to_string(),
    };
    {
        let _lock = RepoLock::acquire_foreground("test").unwrap();
        cmd::store_installed_skill_unlocked(&repo.store, &result, &local_meta, None).unwrap();
    }
    assert_eq!(load(&id).unwrap(), None);
}

#[test]
fn deleting_a_skill_drops_its_baseline_file_and_base_ref() {
    let repo = test_repo();
    let dir = central_repo::skills_dir().join("doomed");
    write_files(&dir, &[("SKILL.md", "---\nname: doomed\n---\n")]);
    repo.store
        .insert_skill(&skill_record("doomed", &dir, "git", Some(REV1)))
        .unwrap();
    record("doomed", REV1, Origin::Install, &dir).unwrap();
    cmd::delete_managed_skills_by_ids(&repo.store, &["doomed".to_string()]).unwrap();
    assert_eq!(load("doomed").unwrap(), None);
    let store = Repository::open_bare(base_store_path()).unwrap();
    assert!(store.find_reference("refs/baselines/doomed").is_err());
}

#[test]
fn sweep_drops_base_refs_of_gone_skills() {
    let repo = test_repo();
    let dir = central_repo::skills_dir().join("alive");
    write_files(&dir, &[("SKILL.md", "---\nname: alive\n---\n")]);
    repo.store
        .insert_skill(&skill_record("alive", &dir, "git", Some(REV1)))
        .unwrap();
    record("alive", REV1, Origin::Install, &dir).unwrap();
    build("ghost", REV1, Origin::Install, &dir).unwrap();
    sweep_base_store(&repo.store);
    let store = Repository::open_bare(base_store_path()).unwrap();
    assert!(store.find_reference("refs/baselines/alive").is_ok());
    assert!(store.find_reference("refs/baselines/ghost").is_err());
}

#[test]
fn rebuild_skips_skills_it_cannot_backfill_and_writes_nothing() {
    let repo = test_repo();
    for (id, source_type) in [("no-rev", "git"), ("local", "local")] {
        let dir = central_repo::skills_dir().join(id);
        write_files(&dir, &[("SKILL.md", "---\nname: a\n---\n")]);
        repo.store
            .insert_skill(&skill_record(id, &dir, source_type, None))
            .unwrap();
    }
    let report = rebuild(
        &repo.store,
        &["no-rev".to_string(), "local".to_string()],
        false,
        false,
        None,
    )
    .unwrap();
    assert_eq!(report.skipped, 2, "{report:?}");
    assert_eq!(report.rebuilt + report.unreachable + report.failed, 0);
    assert_eq!(load("no-rev").unwrap(), None);
    // --all only looks at git skills.
    let all = rebuild(&repo.store, &[], true, false, None).unwrap();
    assert_eq!(all.skills.len(), 1);
    assert_eq!(all.skills[0].skill_id, "no-rev");
}

// ── end to end against a local upstream ──

const PROBE_ENV: &str = "SKILLS_MANAGER_BASELINE_PROBE";
const FAKE_HOST: &str = "https://baseline-test.invalid/";

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
        format!("file://{raw}/")
    } else {
        format!("file:///{raw}/")
    }
}

#[test]
fn install_update_set_source_and_backfill_end_to_end() {
    let root = tempdir().unwrap();
    let name = concat!(module_path!(), "::end_to_end_probe")
        .split_once("::")
        .unwrap()
        .1;
    let output = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            name,
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(PROBE_ENV, root.path())
        .env("GIT_CONFIG_COUNT", "1")
        .env(
            "GIT_CONFIG_KEY_0",
            format!("url.{}.insteadOf", file_url(root.path())),
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

/// Library changes, as `git status` paths, that are neither the skill's own
/// directory nor metadata: there must be none. Leftover staging or temp
/// files would show up here too.
fn stray_library_changes(skills: &Path, skill_dir: &str) -> Vec<String> {
    // Not `git_in`: its trim would eat the first line's leading status column.
    let out = Command::new("git")
        .arg("-C")
        .arg(skills)
        .args(["status", "--porcelain", "--untracked-files=all"])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|line| line[3..].trim_matches('"').to_string())
        .filter(|path| {
            !(path.starts_with(&format!("{skill_dir}/")) || path.starts_with(".skills-manager/"))
                || path.contains(".staged-")
                || path.contains(".tmp.")
                || path.contains(".backup-")
        })
        .collect()
}

fn commit_library(skills: &Path) {
    git_in(skills, &["add", "-A"]);
    git_in(skills, &["commit", "-qm", "step", "--allow-empty"]);
}

#[test]
#[ignore = "run by install_update_set_source_and_backfill_end_to_end in a child process"]
fn end_to_end_probe() {
    let Some(root) = std::env::var_os(PROBE_ENV).map(PathBuf::from) else {
        return;
    };
    let repo = test_repo();
    let skills = central_repo::skills_dir();
    git_in(&skills, &["init", "-q", "-b", "main"]);

    // Upstream: one repo holding `skills/demo` and `skills/demo2`.
    let up = root.join("up.git-src");
    write_files(
        &up,
        &[
            ("skills/demo/SKILL.md", "---\nname: demo\n---\nv1\n"),
            ("skills/demo/refs/notes.md", "notes\n"),
            ("skills/demo2/SKILL.md", "---\nname: demo\n---\nother\n"),
        ],
    );
    git_in(&up, &["init", "-q", "-b", "main"]);
    git_in(&up, &["config", "uploadpack.allowFilter", "true"]);
    git_in(&up, &["add", "-A"]);
    git_in(&up, &["commit", "-qm", "v1"]);
    let rev1 = git_in(&up, &["rev-parse", "HEAD"]);
    let url = format!("{FAKE_HOST}up.git-src");

    // ── install: what the install command does, at rev1 ──
    let checkout =
        git_fetcher::clone_repo_ref_scoped(&url, None, Some("skills/demo"), None, None, None)
            .unwrap();
    git_fetcher::checkout_revision(&checkout, &rev1).unwrap();
    let dest = skills.join("demo");
    let result =
        installer::install_skill_dir_to_destination(&checkout.join("skills/demo"), "demo", &dest)
            .unwrap();
    git_fetcher::cleanup_temp(&checkout);
    let meta = cmd::InstallSourceMetadata {
        source_type: "git".to_string(),
        source_ref: Some(url.clone()),
        source_ref_resolved: Some(url.clone()),
        source_subpath: Some("skills/demo".to_string()),
        source_branch: None,
        source_revision: Some(rev1.clone()),
        remote_revision: Some(rev1.clone()),
        update_status: "up_to_date".to_string(),
    };
    let id = {
        let _lock = RepoLock::acquire_foreground("test").unwrap();
        cmd::store_installed_skill_unlocked(&repo.store, &result, &meta, None).unwrap()
    };
    let installed = load(&id).unwrap().expect("install baseline");
    assert_eq!(installed.revision, rev1);
    assert_eq!(installed.files, manifest_of(&dest).unwrap());
    assert_eq!(stray_library_changes(&skills, "demo"), Vec::<String>::new());
    commit_library(&skills);

    // ── update to rev2 ──
    write_files(
        &up,
        &[("skills/demo/SKILL.md", "---\nname: demo\n---\nv2\n")],
    );
    git_in(&up, &["commit", "-qam", "v2"]);
    let rev2 = git_in(&up, &["rev-parse", "HEAD"]);
    let updated = cmd::update_git_skill_internal(&repo.store, &id, None, None, None).unwrap();
    assert!(updated.content_changed);
    let row = repo.store.get_skill_by_id(&id).unwrap().unwrap();
    assert_eq!(row.source_revision.as_deref(), Some(rev2.as_str()));
    let after_update = load(&id).unwrap().expect("update baseline");
    assert_eq!(after_update.revision, rev2);
    assert_eq!(after_update.origin, Origin::Update);
    assert_eq!(after_update.files, manifest_of(&dest).unwrap());
    assert_eq!(stray_library_changes(&skills, "demo"), Vec::<String>::new());
    commit_library(&skills);

    // ── set-source to another subpath with different content (forced) ──
    write_files(&up, &[("skills/demo2/extra.md", "extra\n")]);
    git_in(&up, &["add", "-A"]);
    git_in(&up, &["commit", "-qm", "v3"]);
    let rev3 = git_in(&up, &["rev-parse", "HEAD"]);
    let set = cmd::set_git_source_internal(
        &repo.store,
        &id,
        &url,
        Some("skills/demo2"),
        None,
        None,
        true,
        false,
    )
    .unwrap();
    assert!(set.content_changed);
    let row = repo.store.get_skill_by_id(&id).unwrap().unwrap();
    assert_eq!(row.source_revision.as_deref(), Some(rev3.as_str()));
    let after_set = load(&id).unwrap().expect("set-source baseline");
    assert_eq!(after_set.revision, rev3);
    assert_eq!(after_set.files, manifest_of(&dest).unwrap());
    commit_library(&skills);

    // ── set-source again where nothing changed: still recorded at the new rev ──
    git_in(&up, &["commit", "-qm", "empty", "--allow-empty"]);
    let rev4 = git_in(&up, &["rev-parse", "HEAD"]);
    let same = cmd::set_git_source_internal(
        &repo.store,
        &id,
        &url,
        Some("skills/demo2"),
        None,
        None,
        false,
        false,
    )
    .unwrap();
    assert!(!same.content_changed);
    let after_same = load(&id).unwrap().unwrap();
    assert_eq!(after_same.revision, rev4);
    assert_eq!(after_same.files, after_set.files);
    commit_library(&skills);

    // ── backfill reproduces what the install path recorded at the same rev ──
    forget(&id).unwrap();
    let report = rebuild(&repo.store, &[id.clone()], false, false, None).unwrap();
    assert_eq!(report.rebuilt, 1, "{report:?}");
    assert_eq!(report.skills[0].differing_files, Some(0));
    let backfilled = load(&id).unwrap().unwrap();
    assert_eq!(backfilled.origin, Origin::Backfill);
    assert_eq!(backfilled.revision, after_same.revision);
    assert_eq!(backfilled.files, after_same.files);
    assert_eq!(backfilled.content_hash, after_same.content_hash);
    assert_eq!(backfilled.tree_id, after_same.tree_id);
    assert_eq!(stray_library_changes(&skills, "demo"), Vec::<String>::new());
    commit_library(&skills);

    // A local edit shows up in the count.
    fs::write(dest.join("extra.md"), "edited\n").unwrap();
    let report = rebuild(&repo.store, &[id.clone()], false, false, None).unwrap();
    assert_eq!(report.skills[0].differing_files, Some(1), "{report:?}");
    git_in(&skills, &["checkout", "--", "."]);

    // ── a revision the server no longer has: reported, nothing written ──
    let gone = "0123456789abcdef0123456789abcdef01234567";
    repo.store
        .update_skill_source_metadata(&id, Some(&url), Some("skills/demo2"), None, Some(gone))
        .unwrap();
    forget(&id).unwrap();
    let report = rebuild(&repo.store, &[id.clone()], false, false, None).unwrap();
    assert_eq!(report.unreachable, 1, "{report:?}");
    assert!(
        report.skills[0]
            .message
            .as_deref()
            .unwrap()
            .contains("not our ref"),
        "{report:?}"
    );
    assert_eq!(load(&id).unwrap(), None);

    // ── any other fetch failure is "could not fetch now", not "gone" ──
    let missing = format!("{FAKE_HOST}missing.git");
    repo.store
        .update_skill_source_metadata(&id, Some(&missing), Some("skills/demo2"), None, Some(&rev4))
        .unwrap();
    let report = rebuild(&repo.store, &[id.clone()], false, false, None).unwrap();
    assert_eq!(report.failed, 1, "{report:?}");
    assert_eq!(report.unreachable, 0, "{report:?}");
    assert_eq!(load(&id).unwrap(), None);

    // ── --all: one rebuilt, one skipped (already has one), one unreachable ──
    repo.store
        .update_skill_source_metadata(&id, Some(&url), Some("skills/demo2"), None, Some(&rev4))
        .unwrap();
    let other = skills.join("other");
    write_files(&other, &[("SKILL.md", "---\nname: other\n---\n")]);
    let mut rec = skill_record("other", &other, "git", Some(gone));
    rec.source_ref = Some(url.clone());
    rec.source_ref_resolved = Some(url.clone());
    rec.source_subpath = Some("skills/demo".to_string());
    repo.store.insert_skill(&rec).unwrap();
    let third = skills.join("third");
    write_files(&third, &[("SKILL.md", "---\nname: third\n---\n")]);
    repo.store
        .insert_skill(&skill_record("third", &third, "git", Some(REV1)))
        .unwrap();
    record("third", REV1, Origin::Install, &third).unwrap();
    let all = rebuild(&repo.store, &[], true, false, None).unwrap();
    assert_eq!(
        (all.rebuilt, all.skipped, all.unreachable, all.failed),
        (1, 1, 1, 0),
        "{all:?}"
    );
    assert_eq!(load("other").unwrap(), None);
    assert_eq!(load("third").unwrap().unwrap().origin, Origin::Install);
}
