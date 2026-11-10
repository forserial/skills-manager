//! Unit tests: the §4 plan row by row, the merged tree, and the pending
//! record. The end-to-end tests through `skills update`, driven from local
//! git repositories, are in `e2e_tests.rs`.

use super::*;
use crate::core::baseline::{folded_sha256, BaselineFile};
use crate::core::central_repo;
use std::fs;
use tempfile::{tempdir, TempDir};

fn write(dir: &Path, rel: &str, body: &[u8]) {
    let path = dir.join(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, body).unwrap();
}

fn tree(root: &TempDir, name: &str, files: &[(&str, &[u8])]) -> PathBuf {
    let dir = root.path().join(name);
    fs::create_dir_all(&dir).unwrap();
    for (rel, body) in files {
        write(&dir, rel, body);
    }
    dir
}

fn manifest(files: &[(&str, &[u8])]) -> Vec<BaselineFile> {
    files
        .iter()
        .map(|(path, body)| BaselineFile {
            path: path.to_string(),
            sha256: folded_sha256(body),
        })
        .collect()
}

/// The action for `f` given its base, local and upstream bodies (`None` =
/// absent). Text unless a body has a NUL.
fn row(base: Option<&[u8]>, local: Option<&[u8]>, upstream: Option<&[u8]>) -> FileAction {
    let side = |body: Option<&[u8]>| -> Vec<BaselineFile> {
        body.map(|b| manifest(&[("f", b)])).unwrap_or_default()
    };
    let text = local
        .zip(upstream)
        .is_none_or(|(l, u)| plan::is_text(l) && plan::is_text(u));
    let plan = plan_manifests(&side(base), &side(local), &side(upstream), |_| Ok(text)).unwrap();
    plan.action_for("f").expect("the path is planned")
}

// ── §4, one test per row ──

const B: &[u8] = b"base\n";
const L: &[u8] = b"local\n";
const U: &[u8] = b"upstream\n";

#[test]
fn row_unchanged_unchanged_keeps() {
    assert_eq!(row(Some(B), Some(B), Some(B)), FileAction::Keep);
}

#[test]
fn row_changed_unchanged_takes_upstream() {
    assert_eq!(row(Some(B), Some(B), Some(U)), FileAction::TakeUpstream);
}

#[test]
fn row_unchanged_modified_keeps_local() {
    assert_eq!(row(Some(B), Some(L), Some(B)), FileAction::KeepLocal);
}

#[test]
fn row_changed_modified_to_the_same_content_takes_either() {
    assert_eq!(row(Some(B), Some(U), Some(U)), FileAction::TakeEither);
    // Line endings aside: the same content.
    assert_eq!(
        row(Some(B), Some(b"upstream\r\n"), Some(U)),
        FileAction::TakeEither
    );
}

#[test]
fn row_changed_modified_differently_text_merges() {
    assert_eq!(row(Some(B), Some(L), Some(U)), FileAction::Merge);
}

#[test]
fn row_changed_modified_differently_binary_holds() {
    assert_eq!(
        row(Some(b"\0base"), Some(b"\0local"), Some(b"\0upstream")),
        FileAction::Hold(ConflictKind::Binary)
    );
}

#[test]
fn row_deleted_unchanged_deletes() {
    assert_eq!(row(Some(B), Some(B), None), FileAction::Delete);
}

#[test]
fn row_deleted_modified_holds() {
    assert_eq!(
        row(Some(B), Some(L), None),
        FileAction::Hold(ConflictKind::DeletedUpstream)
    );
}

#[test]
fn row_unchanged_deleted_stays_deleted() {
    assert_eq!(row(Some(B), None, Some(B)), FileAction::StaysDeleted);
}

#[test]
fn row_changed_deleted_holds() {
    assert_eq!(
        row(Some(B), None, Some(U)),
        FileAction::Hold(ConflictKind::DeletedLocally)
    );
}

#[test]
fn row_added_absent_takes_upstream() {
    assert_eq!(row(None, None, Some(U)), FileAction::TakeUpstream);
}

#[test]
fn row_added_added_differently_holds() {
    assert_eq!(
        row(None, Some(L), Some(U)),
        FileAction::Hold(ConflictKind::AddedBoth)
    );
    assert_eq!(row(None, Some(U), Some(U)), FileAction::TakeEither);
}

#[test]
fn row_absent_added_keeps_local() {
    assert_eq!(row(None, Some(L), None), FileAction::KeepLocal);
}

#[test]
fn deleted_on_both_sides_is_left_out() {
    assert_eq!(row(Some(B), None, None), FileAction::BothDeleted);
}

#[test]
fn a_rename_is_two_paths_never_a_rename() {
    // Renamed locally (old.md → new.md), edited upstream.
    let base = manifest(&[("old.md", b"one\ntwo\n")]);
    let local = manifest(&[("new.md", b"one\ntwo\n")]);
    let upstream = manifest(&[("old.md", b"one\ntwo\nthree\n")]);
    let plan = plan_manifests(&base, &local, &upstream, |_| Ok(true)).unwrap();
    assert_eq!(
        plan.action_for("old.md"),
        Some(FileAction::Hold(ConflictKind::DeletedLocally))
    );
    assert_eq!(plan.action_for("new.md"), Some(FileAction::KeepLocal));
}

// ── merge_text ──

#[test]
fn non_overlapping_edits_merge_and_overlapping_ones_clash() {
    let base = b"one\ntwo\nthree\nfour\nfive\n";
    let local = b"ONE\ntwo\nthree\nfour\nfive\n";
    let upstream = b"one\ntwo\nthree\nfour\nFIVE\n";
    assert_eq!(
        merge_text(base, local, upstream).unwrap().unwrap(),
        b"ONE\ntwo\nthree\nfour\nFIVE\n"
    );
    let clash = b"one\ntwo\nthree\nfour\nfive!\n";
    assert_eq!(merge_text(base, upstream, clash).unwrap(), None);
}

#[test]
fn merge_text_folds_crlf_on_every_input() {
    let base = b"one\ntwo\nthree\nfour\nfive\n";
    let local = b"ONE\r\ntwo\r\nthree\r\nfour\r\nfive\r\n";
    let upstream = b"one\ntwo\nthree\nfour\nFIVE\n";
    // Without folding the whole file would clash.
    assert_eq!(
        merge_text(base, local, upstream).unwrap().unwrap(),
        b"ONE\ntwo\nthree\nfour\nFIVE\n"
    );
}

#[test]
fn crlf_detection_and_restoring() {
    assert!(build::uses_crlf(b"a\r\nb\r\n"));
    assert!(!build::uses_crlf(b"a\r\nb\n"));
    assert!(!build::uses_crlf(b"a\nb\n"));
    assert!(!build::uses_crlf(b"no newline"));
    assert_eq!(build::to_crlf(b"a\nb\n"), b"a\r\nb\r\n");
}

// ── build ──

struct Fixture {
    _root: TempDir,
    base: Vec<BaselineFile>,
    base_files: HashMap<String, Vec<u8>>,
    local: PathBuf,
    upstream: PathBuf,
    staging: PathBuf,
}

fn fixture(base: &[(&str, &[u8])], local: &[(&str, &[u8])], upstream: &[(&str, &[u8])]) -> Fixture {
    let root = tempdir().unwrap();
    Fixture {
        base: manifest(base),
        base_files: base
            .iter()
            .map(|(p, b)| (p.to_string(), b.to_vec()))
            .collect(),
        local: tree(&root, "local", local),
        upstream: tree(&root, "upstream", upstream),
        staging: root.path().join("staging"),
        _root: root,
    }
}

impl Fixture {
    fn build(&self) -> MergeResult {
        let plan = plan(&self.base, &self.local, &self.upstream).unwrap();
        let needed = plan.paths_with(FileAction::Merge);
        let base = BaseContent::Files(
            self.base_files
                .iter()
                .filter(|(p, _)| needed.contains(p))
                .map(|(p, b)| (p.clone(), b.clone()))
                .collect(),
        );
        build(&plan, &base, &self.local, &self.upstream, &self.staging).unwrap()
    }

    fn staged(&self, rel: &str) -> Option<Vec<u8>> {
        fs::read(self.staging.join(rel)).ok()
    }
}

const BASE5: &[u8] = b"one\ntwo\nthree\nfour\nfive\n";

#[test]
fn build_merges_non_overlapping_edits_and_reports_the_file() {
    let fx = fixture(
        &[("SKILL.md", BASE5)],
        &[("SKILL.md", b"ONE\ntwo\nthree\nfour\nfive\n")],
        &[("SKILL.md", b"one\ntwo\nthree\nfour\nFIVE\n")],
    );
    let result = fx.build();
    assert!(result.clean);
    assert_eq!(result.merged, vec!["SKILL.md"]);
    assert_eq!(
        fx.staged("SKILL.md").unwrap(),
        b"ONE\ntwo\nthree\nfour\nFIVE\n"
    );
}

#[test]
fn build_reports_clashing_edits_without_writing_markers() {
    let fx = fixture(
        &[("SKILL.md", BASE5)],
        &[("SKILL.md", b"one\ntwo\nMINE\nfour\nfive\n")],
        &[("SKILL.md", b"one\ntwo\nTHEIRS\nfour\nfive\n")],
    );
    let result = fx.build();
    assert!(!result.clean);
    assert_eq!(result.conflicts.len(), 1);
    let conflict = &result.conflicts[0];
    assert_eq!(
        (conflict.path.as_str(), conflict.kind),
        ("SKILL.md", ConflictKind::Text)
    );
    assert_eq!(
        conflict.local_sha256.as_deref(),
        Some(raw_sha256(b"one\ntwo\nMINE\nfour\nfive\n").as_str())
    );
    let staged = fx.staged("SKILL.md").unwrap();
    assert!(!String::from_utf8_lossy(&staged).contains("<<<<<<<"));
}

#[test]
fn a_crlf_library_file_against_lf_upstream_merges_and_stays_crlf() {
    let fx = fixture(
        &[("SKILL.md", BASE5)],
        &[("SKILL.md", b"ONE\r\ntwo\r\nthree\r\nfour\r\nfive\r\n")],
        &[("SKILL.md", b"one\ntwo\nthree\nfour\nFIVE\n")],
    );
    let result = fx.build();
    assert!(result.clean, "{:?}", result.conflicts);
    assert_eq!(
        fx.staged("SKILL.md").unwrap(),
        b"ONE\r\ntwo\r\nthree\r\nfour\r\nFIVE\r\n"
    );
}

#[test]
fn a_crlf_flip_alone_is_no_change() {
    // Library CRLF (autocrlf), upstream changed another file.
    let fx = fixture(
        &[("SKILL.md", BASE5), ("notes.md", b"v1\n")],
        &[
            ("SKILL.md", b"one\r\ntwo\r\nthree\r\nfour\r\nfive\r\n"),
            ("notes.md", b"v1\n"),
        ],
        &[("SKILL.md", BASE5), ("notes.md", b"v2\n")],
    );
    let plan = plan(&fx.base, &fx.local, &fx.upstream).unwrap();
    assert_eq!(plan.action_for("SKILL.md"), Some(FileAction::Keep));
    let result = fx.build();
    assert!(result.clean);
    assert!(result.merged.is_empty());
    assert_eq!(
        fx.staged("SKILL.md").unwrap(),
        b"one\r\ntwo\r\nthree\r\nfour\r\nfive\r\n",
        "kept byte for byte"
    );
}

#[test]
fn a_binary_changed_on_both_sides_is_a_conflict() {
    let fx = fixture(
        &[("logo.png", b"\x89PNG\0base")],
        &[("logo.png", b"\x89PNG\0mine")],
        &[("logo.png", b"\x89PNG\0theirs")],
    );
    let result = fx.build();
    assert_eq!(result.conflicts[0].kind, ConflictKind::Binary);
}

#[test]
fn a_file_changed_on_one_side_is_that_sides_bytes_line_endings_included() {
    let fx = fixture(
        &[("a.md", b"a\n"), ("b.md", b"b\n")],
        &[("a.md", b"a mine\r\n"), ("b.md", b"b\n")],
        &[("a.md", b"a\n"), ("b.md", b"b theirs\r\n")],
    );
    let result = fx.build();
    assert!(result.clean);
    assert_eq!(fx.staged("a.md").unwrap(), b"a mine\r\n");
    assert_eq!(fx.staged("b.md").unwrap(), b"b theirs\r\n");
}

#[test]
fn local_only_files_survive_and_deletions_follow_the_plan() {
    let fx = fixture(
        &[
            ("SKILL.md", b"s\n"),
            ("gone-upstream.md", b"g\n"),
            ("gone-here.md", b"h\n"),
        ],
        &[
            ("SKILL.md", b"s\n"),
            ("gone-upstream.md", b"g\n"),
            ("mine/notes.md", b"mine\n"),
        ],
        &[
            ("SKILL.md", b"s2\n"),
            ("gone-here.md", b"h\n"),
            ("new.md", b"n\n"),
        ],
    );
    let result = fx.build();
    assert!(result.clean);
    assert_eq!(fx.staged("mine/notes.md").unwrap(), b"mine\n");
    assert_eq!(fx.staged("SKILL.md").unwrap(), b"s2\n");
    assert_eq!(fx.staged("new.md").unwrap(), b"n\n");
    assert_eq!(
        fx.staged("gone-upstream.md"),
        None,
        "deleted upstream: a removal"
    );
    assert_eq!(
        fx.staged("gone-here.md"),
        None,
        "deleted locally: stays deleted"
    );
}

#[test]
fn files_outside_the_content_scope_come_from_upstream() {
    let fx = fixture(
        &[("SKILL.md", b"s\n")],
        &[("SKILL.md", b"mine\n")],
        &[("SKILL.md", b"s\n"), (".gitignore", b"out/\n")],
    );
    fx.build();
    assert_eq!(fx.staged(".gitignore").unwrap(), b"out/\n");
    assert_eq!(fx.staged("SKILL.md").unwrap(), b"mine\n");
}

#[test]
fn a_missing_base_is_a_conflict_never_a_guess() {
    let fx = fixture(
        &[("SKILL.md", BASE5)],
        &[("SKILL.md", b"ONE\ntwo\nthree\nfour\nfive\n")],
        &[("SKILL.md", b"one\ntwo\nthree\nfour\nFIVE\n")],
    );
    let plan = plan(&fx.base, &fx.local, &fx.upstream).unwrap();
    for (base, kind) in [
        (
            BaseContent::Gone("not our ref".into()),
            ConflictKind::BaseGone,
        ),
        (
            BaseContent::Unavailable("timeout".into()),
            ConflictKind::BaseUnavailable,
        ),
    ] {
        let _ = fs::remove_dir_all(&fx.staging);
        let result = build(&plan, &base, &fx.local, &fx.upstream, &fx.staging).unwrap();
        assert_eq!(result.conflicts[0].kind, kind);
        assert!(result.merged.is_empty());
    }
}

// ── the pending record ──

struct Base {
    _lock: std::sync::MutexGuard<'static, ()>,
    _tmp: TempDir,
}

impl Drop for Base {
    fn drop(&mut self) {
        central_repo::set_test_base_dir_override(None);
    }
}

fn isolated_base() -> Base {
    let lock = central_repo::test_base_dir_lock();
    let tmp = tempdir().unwrap();
    central_repo::set_test_base_dir_override(Some(tmp.path().join("base")));
    Base {
        _lock: lock,
        _tmp: tmp,
    }
}

fn skill(id: &str) -> SkillRecord {
    SkillRecord {
        id: id.to_string(),
        name: id.to_string(),
        description: None,
        source_type: "git".to_string(),
        source_ref: None,
        source_ref_resolved: None,
        source_subpath: None,
        source_branch: None,
        source_revision: None,
        remote_revision: None,
        central_path: String::new(),
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

fn text_conflict(path: &str, mine: &[u8], theirs: &[u8]) -> MergeConflict {
    MergeConflict {
        path: path.to_string(),
        kind: ConflictKind::Text,
        local_sha256: Some(raw_sha256(mine)),
        upstream_sha256: Some(raw_sha256(theirs)),
    }
}

fn hold(skill: &SkillRecord, conflicts: &[MergeConflict]) -> PendingMerge {
    pending::save_hold(
        skill,
        "rev2",
        "rev1",
        &["merged.md".to_string()],
        conflicts,
        |_| {
            Ok(pending::Versions {
                base: Some(b"base\n".to_vec()),
                mine: Some(b"mine\n".to_vec()),
                theirs: None,
            })
        },
    )
    .unwrap()
}

#[test]
fn the_pending_record_lives_outside_the_library_and_abort_removes_it() {
    let _base = isolated_base();
    let s = skill("demo");
    let record = hold(&s, &[text_conflict("SKILL.md", b"m", b"t")]);
    let dir = pending::dir_for("demo").unwrap();
    assert!(dir.join("merge.json").is_file());
    assert!(!dir.starts_with(central_repo::skills_dir()));
    assert_eq!(record.unresolved().len(), 1);

    let versions = pending::versions_dir("demo", "SKILL.md").unwrap();
    assert_eq!(fs::read(versions.join("SKILL.mine.md")).unwrap(), b"mine\n");
    assert_eq!(fs::read(versions.join("SKILL.base.md")).unwrap(), b"base\n");
    assert!(!versions.join("SKILL.theirs.md").exists());

    let status = status(&s).unwrap();
    assert!(status.pending);
    assert_eq!(status.unresolved, vec!["SKILL.md"]);
    assert_eq!(status.merged, vec!["merged.md"]);

    assert!(abort(&s).unwrap().aborted);
    assert!(!dir.exists());
    assert!(!abort(&s).unwrap().aborted);
    assert!(!status_of("demo").pending);
}

fn status_of(id: &str) -> MergeStatus {
    status(&skill(id)).unwrap()
}

#[test]
fn a_choice_holds_only_for_the_same_question() {
    let _base = isolated_base();
    let s = skill("demo");
    let conflict = text_conflict("SKILL.md", b"m", b"t");
    hold(&s, &[conflict.clone()]);
    let record = pending::resolve("demo", "SKILL.md", Take::Local, None).unwrap();
    assert_eq!(record.resolution_for("rev1", &conflict), Some(Take::Local));
    // Another baseline, another local copy, or another upstream copy: asked again.
    assert_eq!(record.resolution_for("rev0", &conflict), None);
    assert_eq!(
        record.resolution_for("rev1", &text_conflict("SKILL.md", b"m2", b"t")),
        None
    );
    assert_eq!(
        record.resolution_for("rev1", &text_conflict("SKILL.md", b"m", b"t2")),
        None
    );

    // A new hold over the same question keeps the choice; a changed one drops it.
    let again = hold(&s, &[conflict.clone()]);
    assert!(again.unresolved().is_empty());
    let changed = hold(&s, &[text_conflict("SKILL.md", b"m", b"t3")]);
    assert_eq!(changed.unresolved().len(), 1);
}

#[test]
fn resolving_with_a_file_refuses_conflict_markers_and_unknown_paths() {
    let _base = isolated_base();
    let s = skill("demo");
    hold(&s, &[text_conflict("SKILL.md", b"m", b"t")]);
    let tmp = tempdir().unwrap();
    let marked = tmp.path().join("marked.md");
    fs::write(&marked, "a\n<<<<<<< mine\nb\n=======\nc\n>>>>>>> theirs\n").unwrap();
    assert!(pending::resolve("demo", "SKILL.md", Take::File, Some(&marked)).is_err());
    assert!(pending::resolve("demo", "SKILL.md", Take::File, None).is_err());
    assert!(pending::resolve("demo", "other.md", Take::Local, None).is_err());

    let good = tmp.path().join("good.md");
    fs::write(&good, "Title\n=======\nresolved\n").unwrap();
    let record = pending::resolve("demo", "SKILL.md", Take::File, Some(&good)).unwrap();
    assert!(record.unresolved().is_empty());
    assert_eq!(
        pending::supplied_content("demo", "SKILL.md").unwrap(),
        b"Title\n=======\nresolved\n"
    );
    // Without a pending merge there is nothing to resolve.
    pending::remove("demo").unwrap();
    assert!(pending::resolve("demo", "SKILL.md", Take::Local, None).is_err());
}

#[test]
fn version_names_keep_the_extension() {
    assert_eq!(
        pending::version_file_name("SKILL.md", "mine"),
        "SKILL.mine.md"
    );
    assert_eq!(
        pending::version_file_name("dir/Makefile", "theirs"),
        "Makefile.theirs"
    );
    assert_eq!(pending::version_file_name(".env", "base"), ".env.base");
}

#[test]
fn conflict_kinds_serialize_in_snake_case() {
    assert_eq!(
        serde_json::to_value(ConflictKind::DeletedUpstream).unwrap(),
        "deleted_upstream"
    );
    assert_eq!(serde_json::to_value(Take::Upstream).unwrap(), "upstream");
}
