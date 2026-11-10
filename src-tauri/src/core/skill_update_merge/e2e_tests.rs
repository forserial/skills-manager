//! End to end: install from a local git repository, edit the library copy,
//! move upstream, update, and look at the library, the baseline and the
//! pending record. No network.
//!
//! The update command refuses `file://` URLs, so each test runs a probe in a
//! child test process whose git is told, through `GIT_CONFIG_*` in the
//! child's environment only, to fetch a made-up https host from a local
//! directory (the arrangement the baseline, pinning and overwrite tests use).

use super::*;
use crate::commands::skills::{self as cmd, UpdateSkillResult};
use crate::core::baseline::{self, Origin};
use crate::core::repo_lock::RepoLock;
use crate::core::skill_store::PendingConflictRow;
use crate::core::{central_repo, git_fetcher, skill_auto_updater};
use std::fs;
use std::process::Command;
use std::sync::MutexGuard;
use tempfile::{tempdir, TempDir};

const PROBE_ENV: &str = "SKILLS_MANAGER_MERGE_PROBE";
const ROW_ENV: &str = "SKILLS_MANAGER_MERGE_ROW";
const FAKE_HOST: &str = "https://merge-test.invalid/";

struct TestRepo {
    _lock: MutexGuard<'static, ()>,
    _tmp: TempDir,
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
        _tmp: tmp,
        store,
    }
}

fn write(dir: &Path, rel: &str, body: &[u8]) {
    let path = dir.join(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, body).unwrap();
}

fn git_in(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
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

/// Run `probe` (an ignored test in this module) in a child process whose git
/// maps `FAKE_HOST` onto a fresh directory.
fn run_probe(probe: &str, row: Option<&str>) {
    let root = tempdir().unwrap();
    let name = format!("{}::{probe}", module_path!().split_once("::").unwrap().1);
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            &name,
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(PROBE_ENV, root.path())
        .env("GIT_CONFIG_COUNT", "1")
        .env(
            "GIT_CONFIG_KEY_0",
            format!("url.{}/.insteadOf", file_url(root.path())),
        )
        .env("GIT_CONFIG_VALUE_0", FAKE_HOST);
    if let Some(row) = row {
        command.env(ROW_ENV, row);
    }
    let output = command.output().unwrap();
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

/// An upstream repository `<name>` holding one skill at its root.
struct Upstream {
    dir: PathBuf,
    url: String,
}

impl Upstream {
    fn new(root: &Path, name: &str, files: &[(&str, &[u8])]) -> Self {
        let dir = root.join(name);
        write(&dir, "SKILL.md", b"---\nname: demo\n---\nA demo skill.\n");
        for (rel, body) in files {
            write(&dir, rel, body);
        }
        git_in(&dir, &["init", "-q", "-b", "main"]);
        git_in(&dir, &["config", "uploadpack.allowFilter", "true"]);
        git_in(&dir, &["add", "-A"]);
        git_in(&dir, &["commit", "-qm", "init"]);
        Self {
            url: format!("{FAKE_HOST}{name}"),
            dir,
        }
    }

    fn head(&self) -> String {
        git_in(&self.dir, &["rev-parse", "HEAD"])
    }

    /// Write and delete files, then commit. Returns the new revision.
    fn commit(&self, writes: &[(&str, &[u8])], deletes: &[&str]) -> String {
        for (rel, body) in writes {
            write(&self.dir, rel, body);
        }
        for rel in deletes {
            fs::remove_file(self.dir.join(rel)).unwrap();
        }
        git_in(&self.dir, &["add", "-A"]);
        git_in(&self.dir, &["commit", "-qm", "change"]);
        self.head()
    }
}

/// Install the upstream skill at its head as `name`, as the install command does.
fn install(store: &SkillStore, up: &Upstream, name: &str) -> (String, PathBuf) {
    let rev = up.head();
    let checkout =
        git_fetcher::clone_repo_ref_scoped(&up.url, None, None, None, None, None).unwrap();
    git_fetcher::checkout_revision(&checkout, &rev).unwrap();
    let dest = central_repo::skills_dir().join(name);
    let result = installer::install_skill_dir_to_destination(&checkout, name, &dest).unwrap();
    git_fetcher::cleanup_temp(&checkout);
    let meta = cmd::InstallSourceMetadata {
        source_type: "git".to_string(),
        source_ref: Some(up.url.clone()),
        source_ref_resolved: Some(up.url.clone()),
        source_subpath: None,
        source_branch: None,
        source_revision: Some(rev.clone()),
        remote_revision: Some(rev),
        update_status: "up_to_date".to_string(),
    };
    let id = {
        let _lock = RepoLock::acquire_foreground("test").unwrap();
        cmd::store_installed_skill_unlocked(store, &result, &meta, None).unwrap()
    };
    (id, dest)
}

/// What a person's update does: merge.
fn update(store: &SkillStore, id: &str, approval: Option<&str>) -> UpdateSkillResult {
    cmd::update_git_skill_with(
        store,
        id,
        None,
        None,
        approval,
        MergeMode::WhenLocallyChanged,
    )
    .unwrap()
}

/// Every file under `dir` with its bytes, for byte-for-byte comparisons.
fn snapshot(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    walkdir::WalkDir::new(dir)
        .into_iter()
        .flatten()
        .filter(|e| e.file_type().is_file())
        .map(|e| {
            (
                e.path()
                    .strip_prefix(dir)
                    .unwrap()
                    .display()
                    .to_string()
                    .replace('\\', "/"),
                fs::read(e.path()).unwrap(),
            )
        })
        .collect()
}

/// Nothing named after a staged directory is left in the library.
fn no_staged_leftovers() {
    let leftovers: Vec<String> = fs::read_dir(central_repo::skills_dir())
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n.contains("staged"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
}

fn read(dir: &Path, rel: &str) -> Option<Vec<u8>> {
    fs::read(dir.join(rel)).ok()
}

fn conflict_kinds(result: &UpdateSkillResult) -> Vec<(String, ConflictKind)> {
    result
        .merge
        .as_ref()
        .map(|m| {
            m.conflicts
                .iter()
                .map(|c| (c.path.clone(), c.kind))
                .collect()
        })
        .unwrap_or_default()
}

// ── one test per row of design §4, through `skills update` ──
//
// Each row is the file `f.md` (or `f.bin`). A local-only `mine.md` makes
// every skill locally changed, so the update always takes the merge path,
// and an upstream change to `other.md` moves upstream in the rows where
// upstream leaves the file alone.

macro_rules! row_test {
    ($name:ident) => {
        #[test]
        fn $name() {
            run_probe("row_probe", Some(stringify!($name)));
        }
    };
}

row_test!(row_unchanged_unchanged_keep);
row_test!(row_changed_unchanged_take_upstream);
row_test!(row_unchanged_modified_keep_local);
row_test!(row_changed_modified_same_take_either);
row_test!(row_changed_modified_text_merge);
row_test!(row_changed_modified_binary_hold);
row_test!(row_deleted_unchanged_delete_with_approval);
row_test!(row_deleted_modified_hold);
row_test!(row_unchanged_deleted_stays_deleted);
row_test!(row_changed_deleted_hold);
row_test!(row_added_absent_take_upstream);
row_test!(row_added_added_hold);
row_test!(row_absent_added_keep_local);

struct Row<'a> {
    base: Option<&'a [u8]>,
    local: Option<&'a [u8]>,
    upstream: Option<&'a [u8]>,
    file: &'a str,
}

const FIVE: &[u8] = b"one\ntwo\nthree\nfour\nfive\n";

fn row_spec(name: &str) -> Row<'static> {
    let text = |base, local, upstream| Row {
        base,
        local,
        upstream,
        file: "f.md",
    };
    match name {
        "row_unchanged_unchanged_keep" => text(Some(FIVE), Some(FIVE), Some(FIVE)),
        "row_changed_unchanged_take_upstream" => text(Some(FIVE), Some(FIVE), Some(b"up\n")),
        "row_unchanged_modified_keep_local" => text(Some(FIVE), Some(b"mine\r\n"), Some(FIVE)),
        "row_changed_modified_same_take_either" => {
            text(Some(FIVE), Some(b"same\r\n"), Some(b"same\n"))
        }
        "row_changed_modified_text_merge" => text(
            Some(FIVE),
            Some(b"ONE\ntwo\nthree\nfour\nfive\n"),
            Some(b"one\ntwo\nthree\nfour\nFIVE\n"),
        ),
        "row_changed_modified_binary_hold" => Row {
            base: Some(b"\0base"),
            local: Some(b"\0mine"),
            upstream: Some(b"\0theirs"),
            file: "f.bin",
        },
        "row_deleted_unchanged_delete_with_approval" => text(Some(FIVE), Some(FIVE), None),
        "row_deleted_modified_hold" => text(Some(FIVE), Some(b"mine\n"), None),
        "row_unchanged_deleted_stays_deleted" => text(Some(FIVE), None, Some(FIVE)),
        "row_changed_deleted_hold" => text(Some(FIVE), None, Some(b"up\n")),
        "row_added_absent_take_upstream" => text(None, None, Some(b"new\n")),
        "row_added_added_hold" => text(None, Some(b"mine\n"), Some(b"theirs\n")),
        "row_absent_added_keep_local" => text(None, Some(b"mine\n"), None),
        other => panic!("unknown row {other}"),
    }
}

#[test]
#[ignore = "run by the row_* tests in a child process"]
fn row_probe() {
    let Some(root) = probe_root() else { return };
    let name = std::env::var(ROW_ENV).unwrap();
    let spec = row_spec(&name);
    let repo = test_repo();

    let mut files: Vec<(&str, &[u8])> = vec![("other.md", b"other v1\n")];
    if let Some(base) = spec.base {
        files.push((spec.file, base));
    }
    let up = Upstream::new(&root, "up", &files);
    let (id, dest) = install(&repo.store, &up, "demo");

    // Local side.
    write(&dest, "mine.md", b"local only\n");
    match spec.local {
        Some(body) if Some(body) != spec.base => write(&dest, spec.file, body),
        Some(_) => {}
        None if spec.base.is_some() => fs::remove_file(dest.join(spec.file)).unwrap(),
        None => {}
    }
    // Upstream side, always moving.
    let mut writes: Vec<(&str, &[u8])> = vec![("other.md", b"other v2\n")];
    let mut deletes = Vec::new();
    match spec.upstream {
        Some(body) if Some(body) != spec.base => writes.push((spec.file, body)),
        Some(_) => {}
        None if spec.base.is_some() => deletes.push(spec.file),
        None => {}
    }
    let rev2 = up.commit(&writes, &deletes);
    let before = snapshot(&dest);

    let result = update(&repo.store, &id, None);
    let held_kind = match name.as_str() {
        "row_changed_modified_binary_hold" => Some(ConflictKind::Binary),
        "row_deleted_modified_hold" => Some(ConflictKind::DeletedUpstream),
        "row_changed_deleted_hold" => Some(ConflictKind::DeletedLocally),
        "row_added_added_hold" => Some(ConflictKind::AddedBoth),
        _ => None,
    };
    if let Some(kind) = held_kind {
        assert!(result.is_held());
        assert_eq!(conflict_kinds(&result), vec![(spec.file.to_string(), kind)]);
        assert!(!result.content_changed);
        assert_eq!(snapshot(&dest), before, "a held merge changes nothing");
        no_staged_leftovers();
        assert!(pending::load(&id).unwrap().is_some());
        assert_ne!(baseline::load(&id).unwrap().unwrap().revision, rev2);
        return;
    }

    if name == "row_deleted_unchanged_delete_with_approval" {
        // Still listed for approval, as today.
        assert!(result.is_held());
        let removed: Vec<&str> = result
            .pending_removals
            .iter()
            .map(|p| p.path.as_str())
            .collect();
        assert_eq!(removed, vec!["f.md"]);
        assert_eq!(snapshot(&dest), before);
        let result = update(&repo.store, &id, result.removal_approval.as_deref());
        assert!(!result.is_held());
        assert_eq!(read(&dest, "f.md"), None);
    } else {
        assert!(!result.is_held(), "{:?}", result.merge);
        assert!(result.pending_removals.is_empty(), "no approval prompt");
        assert!(result.pending_overwrites.is_empty());
        let expected: Option<&[u8]> = match name.as_str() {
            "row_unchanged_unchanged_keep" => spec.local,
            "row_changed_unchanged_take_upstream" => spec.upstream,
            "row_unchanged_modified_keep_local" => spec.local,
            "row_changed_modified_same_take_either" => spec.local,
            "row_changed_modified_text_merge" => Some(b"ONE\ntwo\nthree\nfour\nFIVE\n"),
            "row_unchanged_deleted_stays_deleted" => None,
            "row_added_absent_take_upstream" => spec.upstream,
            "row_absent_added_keep_local" => spec.local,
            other => panic!("no expectation for {other}"),
        };
        assert_eq!(read(&dest, spec.file).as_deref(), expected, "{name}");
        if name == "row_changed_modified_text_merge" {
            assert_eq!(result.merge.as_ref().unwrap().merged, vec!["f.md"]);
        }
    }
    // Every applied row: upstream's other change landed, the local-only file
    // survived, and the baseline is the new upstream manifest.
    assert_eq!(read(&dest, "other.md").unwrap(), b"other v2\n");
    assert_eq!(read(&dest, "mine.md").unwrap(), b"local only\n");
    let recorded = baseline::load(&id).unwrap().unwrap();
    assert_eq!(recorded.revision, rev2);
    assert_eq!(recorded.origin, Origin::Merge);
    assert!(!recorded.files.iter().any(|f| f.path == "mine.md"));
    no_staged_leftovers();
    assert!(pending::load(&id).unwrap().is_none());
}

// ── the acceptance scenarios ──

/// The library as a git repository, as the backup makes it.
fn init_library_git() {
    let lib = central_repo::skills_dir();
    git_in(&lib, &["init", "-q"]);
    git_in(&lib, &["add", "-A"]);
    git_in(&lib, &["commit", "-qm", "library", "--allow-empty"]);
}

/// Changed paths in the library, from `git status --porcelain` (untrimmed:
/// the status columns carry meaning).
fn library_status() -> Vec<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(central_repo::skills_dir())
        .args(["status", "--porcelain", "-uall"])
        .output()
        .unwrap();
    assert!(out.status.success());
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| l[3..].trim_matches('"').to_string())
        .collect()
}

fn commit_library() {
    let lib = central_repo::skills_dir();
    git_in(&lib, &["add", "-A"]);
    git_in(&lib, &["commit", "-qm", "after", "--allow-empty"]);
}

#[test]
fn a_local_edit_and_an_unrelated_upstream_edit_to_the_same_file_merge() {
    run_probe("clean_merge_probe", None);
}

#[test]
#[ignore = "run by a_local_edit_and_an_unrelated_upstream_edit_to_the_same_file_merge"]
fn clean_merge_probe() {
    let Some(root) = probe_root() else { return };
    let repo = test_repo();
    let body = "---\nname: demo\n---\nintro\n\nalpha\nbeta\ngamma\ndelta\n\nlast\n";
    let up = Upstream::new(&root, "up", &[("SKILL.md", body.as_bytes())]);
    let (id, dest) = install(&repo.store, &up, "demo");
    let (other, _) = install(&repo.store, &up, "other");

    // A one-line local edit; upstream edits another part of the same file.
    write(
        &dest,
        "SKILL.md",
        body.replace("intro", "intro, my way").as_bytes(),
    );
    init_library_git();
    let rev2 = up.commit(
        &[("SKILL.md", body.replace("last", "last, theirs").as_bytes())],
        &[],
    );

    // Unattended callers still hold it: they never merge.
    let before = snapshot(&dest);
    let unattended = cmd::update_git_skill_internal(&repo.store, &id, None, None, None).unwrap();
    assert!(unattended.is_held());
    assert!(unattended.merge.is_none());
    assert_eq!(snapshot(&dest), before);

    let result = update(&repo.store, &id, None);
    assert!(!result.is_held(), "{:?}", result.merge);
    assert!(result.content_changed);
    let report = result.merge.as_ref().unwrap();
    assert_eq!(report.merged, vec!["SKILL.md"]);
    assert_eq!(report.base_source, Some(BaseSource::BaseStore));
    let merged = String::from_utf8(read(&dest, "SKILL.md").unwrap()).unwrap();
    assert!(
        merged.contains("intro, my way") && merged.contains("last, theirs"),
        "{merged}"
    );

    // The baseline is the new upstream manifest; the local edit is still one.
    let recorded = baseline::load(&id).unwrap().unwrap();
    assert_eq!(
        (recorded.revision.as_str(), recorded.origin),
        (rev2.as_str(), Origin::Merge)
    );
    let changes = baseline::local_changes_by_id(&repo.store, &id).unwrap();
    let changed: Vec<&str> = changes.files.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(changed, vec!["SKILL.md"]);
    let row = repo.store.get_skill_by_id(&id).unwrap().unwrap();
    assert_eq!(row.source_revision.as_deref(), Some(rev2.as_str()));
    assert_eq!(row.update_status, "up_to_date");

    // Only this skill's own files changed in the library.
    let changed = library_status();
    assert!(!changed.is_empty());
    for path in &changed {
        assert!(
            path.starts_with("demo/")
                || (path.starts_with(".skills-manager/") && path.contains(&id)),
            "{path} is not the skill's own: {changed:?}"
        );
    }
    assert!(!changed.iter().any(|p| p.contains(&other)));
    commit_library();

    // A second update with no new upstream commits is a no-op.
    let again = update(&repo.store, &id, None);
    assert!(!again.is_held());
    assert!(!again.content_changed);
    assert_eq!(read(&dest, "SKILL.md").unwrap(), merged.as_bytes());
    assert_eq!(baseline::load(&id).unwrap().unwrap(), recorded);
    assert_eq!(library_status(), Vec::<String>::new());

    // One audit entry per merge.
    let audits = repo.store.list_audit(Some(50)).unwrap();
    let merges = audits.iter().filter(|a| a.action == "merge").count();
    assert_eq!(merges, 2);
}

#[test]
fn clashing_edits_hold_until_resolved_then_apply() {
    run_probe("conflict_probe", None);
}

#[test]
#[ignore = "run by clashing_edits_hold_until_resolved_then_apply"]
fn conflict_probe() {
    let Some(root) = probe_root() else { return };
    let repo = test_repo();
    let up = Upstream::new(
        &root,
        "up",
        &[
            ("notes.md", FIVE),
            ("guide.md", b"guide v1\n"),
            ("tips.md", FIVE),
        ],
    );
    let (id, dest) = install(&repo.store, &up, "demo");
    let skill = repo.store.get_skill_by_id(&id).unwrap().unwrap();

    write(&dest, "notes.md", b"one\ntwo\nMINE\nfour\nfive\n");
    write(&dest, "guide.md", b"my guide\n");
    write(&dest, "tips.md", b"ONE\ntwo\nthree\nfour\nfive\n");
    init_library_git();
    let rev2 = up.commit(
        &[
            ("notes.md", b"one\ntwo\nTHEIRS\nfour\nfive\n"),
            ("guide.md", b"their guide\n"),
            ("tips.md", b"one\ntwo\nthree\nfour\nFIVE\n"),
        ],
        &[],
    );
    let before = snapshot(&dest);

    let held = update(&repo.store, &id, None);
    assert!(held.is_held());
    assert!(
        held.removal_approval.is_none(),
        "conflicts are not approved by a token"
    );
    assert_eq!(
        conflict_kinds(&held),
        vec![
            ("guide.md".to_string(), ConflictKind::Text),
            ("notes.md".to_string(), ConflictKind::Text)
        ]
    );
    assert_eq!(held.merge.as_ref().unwrap().merged, vec!["tips.md"]);
    assert_eq!(snapshot(&dest), before, "byte for byte unchanged");
    no_staged_leftovers();
    assert_eq!(
        library_status(),
        Vec::<String>::new(),
        "a held merge leaves nothing"
    );
    assert_eq!(
        repo.store
            .get_skill_by_id(&id)
            .unwrap()
            .unwrap()
            .update_status,
        "update_available"
    );

    // `skills merge status` lists them, with both versions to open.
    let status = status(&skill).unwrap();
    assert!(status.pending);
    assert_eq!(status.target_revision.as_deref(), Some(rev2.as_str()));
    assert_eq!(status.unresolved, vec!["guide.md", "notes.md"]);
    let versions = PathBuf::from(&status.conflicts[1].versions_dir);
    assert_eq!(
        fs::read(versions.join("notes.mine.md")).unwrap(),
        b"one\ntwo\nMINE\nfour\nfive\n"
    );
    assert_eq!(
        fs::read(versions.join("notes.theirs.md")).unwrap(),
        b"one\ntwo\nTHEIRS\nfour\nfive\n"
    );
    assert_eq!(fs::read(versions.join("notes.base.md")).unwrap(), FIVE);

    // Resolving one is not enough.
    resolve(&skill, "guide.md", Take::Upstream, None).unwrap();
    let still = update(&repo.store, &id, None);
    assert_eq!(
        conflict_kinds(&still),
        vec![("notes.md".to_string(), ConflictKind::Text)]
    );
    assert_eq!(still.merge.as_ref().unwrap().resolved, vec!["guide.md"]);
    assert_eq!(snapshot(&dest), before);

    // Resolving the other with supplied content, then apply.
    let resolved = root.join("notes-resolved.md");
    fs::write(&resolved, b"one\ntwo\nBOTH\nfour\nfive\n").unwrap();
    let status = resolve(&skill, "notes.md", Take::File, Some(&resolved)).unwrap();
    assert!(status.unresolved.is_empty());
    require_pending(&skill).unwrap();
    let applied = update(&repo.store, &id, None);
    assert!(!applied.is_held(), "{:?}", applied.merge);
    assert_eq!(
        read(&dest, "notes.md").unwrap(),
        b"one\ntwo\nBOTH\nfour\nfive\n"
    );
    assert_eq!(read(&dest, "guide.md").unwrap(), b"their guide\n");
    assert_eq!(
        read(&dest, "tips.md").unwrap(),
        b"ONE\ntwo\nthree\nfour\nFIVE\n"
    );
    assert!(
        pending::load(&id).unwrap().is_none(),
        "applied: the record is gone"
    );
    assert!(require_pending(&skill).is_err());

    // `skills status`: the new baseline revision and the remaining local changes.
    assert_eq!(baseline::load(&id).unwrap().unwrap().revision, rev2);
    let changes = baseline::local_changes_by_id(&repo.store, &id).unwrap();
    let changed: Vec<&str> = changes.files.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(changed, vec!["notes.md", "tips.md"]);
    for path in library_status() {
        assert!(path.starts_with("demo/") || path.contains(&id), "{path}");
    }
}

#[test]
fn abort_drops_the_pending_merge_and_leaves_the_library_alone() {
    run_probe("abort_probe", None);
}

#[test]
#[ignore = "run by abort_drops_the_pending_merge_and_leaves_the_library_alone"]
fn abort_probe() {
    let Some(root) = probe_root() else { return };
    let repo = test_repo();
    let up = Upstream::new(&root, "up", &[("notes.md", FIVE)]);
    let (id, dest) = install(&repo.store, &up, "demo");
    let skill = repo.store.get_skill_by_id(&id).unwrap().unwrap();
    write(&dest, "notes.md", b"mine\n");
    up.commit(&[("notes.md", b"theirs\n")], &[]);
    let before = snapshot(&dest);
    assert!(update(&repo.store, &id, None).is_held());
    resolve(&skill, "notes.md", Take::Local, None).unwrap();
    assert!(abort(&skill).unwrap().aborted);
    assert!(!pending::dir_for(&id).unwrap().exists());
    assert_eq!(snapshot(&dest), before);
    // The choice went with it: the next update asks again.
    assert!(update(&repo.store, &id, None).is_held());
    no_staged_leftovers();
}

#[test]
fn rule_a_and_unattended_callers_never_merge() {
    run_probe("never_merge_probe", None);
}

#[test]
#[ignore = "run by rule_a_and_unattended_callers_never_merge"]
fn never_merge_probe() {
    let Some(root) = probe_root() else { return };
    let repo = test_repo();
    let up = Upstream::new(&root, "up", &[("notes.md", b"v1\n")]);
    let (id, dest) = install(&repo.store, &up, "demo");
    write(&dest, "SKILL.md", b"---\nname: demo\n---\nmine\n");
    up.commit(&[("notes.md", b"v2\n")], &[]);
    let before = snapshot(&dest);

    // A pending library sync conflict (Rule A): held as suspect, not merged.
    repo.store
        .replace_pending_conflicts(&[PendingConflictRow {
            skill_id: id.clone(),
            theirs_commit: "2".repeat(40),
            theirs_path: None,
            detected_at: 1,
        }])
        .unwrap();
    let suspect = update(&repo.store, &id, None);
    assert!(suspect.is_held());
    assert!(suspect.merge.is_none());
    assert_eq!(
        suspect.overwrite_reason,
        Some(baseline::OverwriteReason::BaselineSuspect)
    );
    assert_eq!(snapshot(&dest), before);
    repo.store.replace_pending_conflicts(&[]).unwrap();

    // The auto-updater and batch update skip the locally changed skill.
    repo.store.set_setting("auto_update_apply", "on").unwrap();
    let round = skill_auto_updater::run_round_blocking(&repo.store).unwrap();
    assert_eq!((round.updated, round.held_back), (0, 1));
    assert_eq!(snapshot(&dest), before);

    // No baseline: never merged either.
    baseline::forget(&id).unwrap();
    let unknown = update(&repo.store, &id, None);
    assert!(unknown.merge.is_none());
    assert_eq!(
        unknown.overwrite_reason,
        Some(baseline::OverwriteReason::BaselineNotBackfilled)
    );
    assert_eq!(snapshot(&dest), before);
    no_staged_leftovers();
}

#[test]
fn base_content_is_fetched_by_sha_and_a_gone_revision_holds() {
    run_probe("base_fetch_probe", None);
}

#[test]
#[ignore = "run by base_content_is_fetched_by_sha_and_a_gone_revision_holds"]
fn base_fetch_probe() {
    let Some(root) = probe_root() else { return };
    let repo = test_repo();
    let up = Upstream::new(&root, "up", &[("notes.md", FIVE)]);
    let (id, dest) = install(&repo.store, &up, "demo");
    let (gone_id, gone_dest) = install(&repo.store, &up, "gone");
    for dir in [&dest, &gone_dest] {
        write(dir, "notes.md", b"ONE\ntwo\nthree\nfour\nfive\n");
    }
    up.commit(&[("notes.md", b"one\ntwo\nthree\nfour\nFIVE\n")], &[]);
    // A device without the base store (a second device, or a cleared cache).
    fs::remove_dir_all(central_repo::cache_dir().join("baselines.git")).unwrap();

    let fetched = update(&repo.store, &id, None);
    assert!(!fetched.is_held(), "{:?}", fetched.merge);
    assert_eq!(
        fetched.merge.as_ref().unwrap().base_source,
        Some(BaseSource::Fetch)
    );
    assert_eq!(
        read(&dest, "notes.md").unwrap(),
        b"ONE\ntwo\nthree\nfour\nFIVE\n"
    );

    // The baseline revision vanished upstream: hold, and let the person choose.
    let mut recorded = baseline::load(&gone_id).unwrap().unwrap();
    recorded.revision = "0123456789abcdef0123456789abcdef01234567".to_string();
    recorded.tree_id = None;
    crate::core::fork_files::write(
        crate::core::fork_files::ForkConcern::Baseline,
        &gone_id,
        &recorded,
    )
    .unwrap();
    let before = snapshot(&gone_dest);
    let gone = update(&repo.store, &gone_id, None);
    assert_eq!(
        conflict_kinds(&gone),
        vec![("notes.md".to_string(), ConflictKind::BaseGone)]
    );
    assert!(gone.merge.as_ref().unwrap().base_message.is_some());
    assert_eq!(snapshot(&gone_dest), before);
    let skill = repo.store.get_skill_by_id(&gone_id).unwrap().unwrap();
    resolve(&skill, "notes.md", Take::Local, None).unwrap();
    let kept = update(&repo.store, &gone_id, None);
    assert!(!kept.is_held());
    assert_eq!(
        read(&gone_dest, "notes.md").unwrap(),
        b"ONE\ntwo\nthree\nfour\nfive\n"
    );
    no_staged_leftovers();
}
