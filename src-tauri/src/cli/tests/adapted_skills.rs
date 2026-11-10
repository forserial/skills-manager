//! Adapted skills (sm-69q.11): scenarios 1, 2, 4 and 5 of the epic's end-to-end
//! verification, through the CLI's own `skills install`, `skills update` and
//! `skills merge` code rather than the core functions the child issues tested.
//!
//! Each scenario runs as a probe in a child test process: the library root is
//! process-wide state, and the child's git is told, through `GIT_CONFIG_*` in
//! its environment only, to fetch a made-up https host from local
//! repositories. No network, and nothing outside a temp folder is touched.
//!
//! The automatic updater's round is `pub(crate)` in the library and cannot be
//! driven from here; scenario 4's unattended round is covered in
//! `core::baseline::overwrites` and `core::skill_update_merge` (never merges).
//! `skills update --all` is the CLI's unattended path and is tested here.

use super::*;
use app_lib::core::baseline;
use app_lib::core::skill_update_merge::{self as merge, Take};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs;
use std::process::Command;
use tempfile::tempdir;

const PROBE_ENV: &str = "SKILLS_MANAGER_CLI_ADAPTED_PROBE";
const FAKE_HOST: &str = "https://adapted-cli-test.invalid/";

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

/// `file://` wants forward slashes and, on Windows, a slash before the drive.
fn file_url(path: &Path) -> String {
    let raw = path.display().to_string().replace('\\', "/");
    if raw.starts_with('/') {
        format!("file://{raw}")
    } else {
        format!("file:///{raw}")
    }
}

/// Run `probe` (an ignored test in this module) in a child process.
fn run_probe(probe: &str) {
    let root = tempdir().unwrap();
    let upstreams = root.path().join("up");
    fs::create_dir_all(&upstreams).unwrap();
    let name = format!("{}::{probe}", module_path!().split_once("::").unwrap().1);
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", &name, "--ignored", "--nocapture", "--test-threads=1"])
        .env(PROBE_ENV, root.path())
        .env("GIT_CONFIG_COUNT", "1")
        .env("GIT_CONFIG_KEY_0", format!("url.{}/.insteadOf", file_url(&upstreams)))
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

/// The probe's root, or `None` when the test runs outside its child process.
fn probe_root() -> Option<PathBuf> {
    std::env::var_os(PROBE_ENV).map(PathBuf::from)
}

/// A scratch library, opened the way the CLI opens one.
fn open_library(root: &Path) -> Arc<SkillStore> {
    central_repo::set_runtime_base_dir_override(Some(root.join("base")));
    central_repo::set_runtime_skills_dir_override(Some(root.join("skills")));
    app_state::initialize_cli_store().unwrap()
}

const SKILL_MD: &str = "---\nname: NAME\n---\nline one\nline two\nline three\nline four\nline five\n";

/// An upstream repository holding one skill at its root.
struct Upstream {
    dir: PathBuf,
    url: String,
}

impl Upstream {
    fn new(root: &Path, name: &str) -> Self {
        let dir = root.join("up").join(name);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("SKILL.md"), SKILL_MD.replace("NAME", name)).unwrap();
        fs::write(dir.join("notes.md"), "notes v1\n").unwrap();
        git_in(&dir, &["init", "-q", "-b", "main"]);
        git_in(&dir, &["config", "uploadpack.allowFilter", "true"]);
        git_in(&dir, &["add", "-A"]);
        git_in(&dir, &["commit", "-qm", "init"]);
        Self {
            url: format!("{FAKE_HOST}{name}"),
            dir,
        }
    }

    /// Write files and commit. Returns the new revision.
    fn commit(&self, writes: &[(&str, &str)]) -> String {
        for (rel, body) in writes {
            fs::write(self.dir.join(rel), body).unwrap();
        }
        git_in(&self.dir, &["add", "-A"]);
        git_in(&self.dir, &["commit", "-qm", "upstream change"]);
        git_in(&self.dir, &["rev-parse", "HEAD"])
    }
}

/// `skills install <url> --git --name <name>`; returns the library copy.
fn install(store: &SkillStore, up: &Upstream, name: &str) -> PathBuf {
    let report = run_install(store, &up.url, Some(name), InstallKind::Git, SyncTarget::None).unwrap();
    PathBuf::from(report.central_path)
}

/// `skills update <name>` or `skills update --all`, as JSON rows keyed by name.
fn update(store: &SkillStore, name: Option<&str>) -> BTreeMap<String, Value> {
    run_update(store, name, name.is_none())
        .unwrap()
        .into_iter()
        .map(|r| (r.name.clone(), serde_json::to_value(&r).unwrap()))
        .collect()
}

fn status(store: &SkillStore, name: &str) -> Value {
    serde_json::to_value(skill_status(store, name).unwrap()).unwrap()
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap()
}

/// Every file under `dir` with its bytes.
fn snapshot(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    walkdir::WalkDir::new(dir)
        .into_iter()
        .flatten()
        .filter(|e| e.file_type().is_file())
        .map(|e| {
            let rel = e.path().strip_prefix(dir).unwrap().display().to_string();
            (rel.replace('\\', "/"), fs::read(e.path()).unwrap())
        })
        .collect()
}

/// A report that applied with no question asked.
fn assert_applied_without_prompt(row: &Value) {
    assert_eq!(row["refreshed"], true, "{row}");
    assert!(row["error"].is_null(), "{row}");
    for held in ["held_back_removals", "held_back_overwrites", "merge_conflicts", "pinned"] {
        assert!(row.get(held).is_none(), "unexpected {held}: {row}");
    }
}

/// Nothing held, nothing changed: what an up-to-date skill reports.
fn assert_nothing_to_do(row: &Value) {
    assert_eq!(row["refreshed"], false, "{row}");
    assert!(row["error"].is_null(), "{row}");
    for held in ["held_back_removals", "held_back_overwrites", "merge_conflicts"] {
        assert!(row.get(held).is_none(), "unexpected {held}: {row}");
    }
}

fn changed_files(status: &Value) -> Vec<(String, String)> {
    status["local_changes"]["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| (f["path"].as_str().unwrap().to_string(), f["state"].as_str().unwrap().to_string()))
        .collect()
}

// ── scenarios 1 and 5 ──

#[test]
fn scenario_1_and_5_an_edit_and_a_local_file_survive_an_update_through_the_cli() {
    run_probe("scenario_1_and_5_probe");
}

#[test]
#[ignore = "run by scenario_1_and_5_an_edit_and_a_local_file_survive_an_update_through_the_cli"]
fn scenario_1_and_5_probe() {
    let Some(root) = probe_root() else { return };
    let store = open_library(&root);
    let up = Upstream::new(&root, "adapted");
    let dir = install(&store, &up, "adapted");

    // Scenario 1: the user edits one file, upstream changes another.
    let edited = format!("{}my local line\n", read(&dir.join("SKILL.md")));
    fs::write(dir.join("SKILL.md"), &edited).unwrap();
    // Scenario 5: a file only the library has.
    fs::create_dir_all(dir.join("local")).unwrap();
    fs::write(dir.join("local/mine.md"), "written by me\n").unwrap();
    let rev2 = up.commit(&[("notes.md", "notes v2\n")]);

    let rows = update(&store, Some("adapted"));
    assert_applied_without_prompt(&rows["adapted"]);
    assert_eq!(read(&dir.join("SKILL.md")), edited, "the edit survives");
    assert_eq!(read(&dir.join("notes.md")), "notes v2\n", "upstream's change lands");
    assert_eq!(read(&dir.join("local/mine.md")), "written by me\n", "the local file survives");

    let after = status(&store, "adapted");
    assert_eq!(after["baseline"]["revision"], rev2.as_str());
    assert_eq!(after["baseline"]["origin"], "merge");
    assert_eq!(
        changed_files(&after),
        vec![
            ("SKILL.md".to_string(), "modified".to_string()),
            ("local/mine.md".to_string(), "added".to_string()),
        ]
    );

    // F1: `update --all` (which never merges) has nothing to take while
    // upstream has not moved, so it neither holds nor writes anything.
    let id = after["id"].as_str().unwrap().to_string();
    let baseline_file = fs::read(fork_file(&root, "baseline", &id)).unwrap();
    let before = snapshot(&dir);
    let rows = update(&store, None);
    assert_nothing_to_do(&rows["adapted"]);
    assert_eq!(snapshot(&dir), before);
    assert_eq!(fs::read(fork_file(&root, "baseline", &id)).unwrap(), baseline_file);
    // And a second named update is a no-op too.
    assert_nothing_to_do(&update(&store, Some("adapted"))["adapted"]);
    assert_eq!(snapshot(&dir), before);
}

fn fork_file(root: &Path, concern: &str, id: &str) -> PathBuf {
    root.join("skills/.skills-manager")
        .join(format!("fork-{concern}.{id}.json"))
}

// ── scenario 2 ──

#[test]
fn scenario_2_a_clash_is_held_then_resolved_and_applied_through_the_cli() {
    run_probe("scenario_2_probe");
}

#[test]
#[ignore = "run by scenario_2_a_clash_is_held_then_resolved_and_applied_through_the_cli"]
fn scenario_2_probe() {
    let Some(root) = probe_root() else { return };
    let store = open_library(&root);
    let up = Upstream::new(&root, "clash");
    let dir = install(&store, &up, "clash");

    let mine = read(&dir.join("SKILL.md")).replace("line one\n", "line one, mine\n");
    fs::write(dir.join("SKILL.md"), &mine).unwrap();
    let theirs = SKILL_MD
        .replace("NAME", "clash")
        .replace("line one\n", "line one, theirs\n");
    let rev2 = up.commit(&[("SKILL.md", &theirs), ("notes.md", "notes v2\n")]);
    let before = snapshot(&dir);

    let row = &update(&store, Some("clash"))["clash"];
    assert_eq!(row["refreshed"], false, "{row}");
    assert!(row["error"].is_null(), "a hold is not an error: {row}");
    let conflicts = row["merge_conflicts"].as_array().unwrap();
    assert_eq!(conflicts.len(), 1, "{row}");
    assert_eq!(conflicts[0]["path"], "SKILL.md");
    assert_eq!(conflicts[0]["kind"], "text");
    assert_eq!(snapshot(&dir), before, "a held merge changes nothing");
    let leftovers: Vec<String> = fs::read_dir(dir.parent().unwrap())
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n.contains("staged"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");

    // `skills merge resolve clash SKILL.md --file <hand merge>`, then apply.
    let skill = resolve_skill(&store, "clash").unwrap();
    let hand = mine.replace("line one, mine\n", "line one, theirs and mine\n");
    let hand_path = root.join("hand.md");
    fs::write(&hand_path, &hand).unwrap();
    let pending = serde_json::to_value(merge::resolve(&skill, "SKILL.md", Take::File, Some(&hand_path)).unwrap()).unwrap();
    assert_eq!(pending["unresolved"].as_array().unwrap().len(), 0, "{pending}");
    merge::require_pending(&skill).unwrap();
    let row = &update(&store, Some("clash"))["clash"]; // what `skills merge apply` runs
    assert_applied_without_prompt(row);
    assert_eq!(read(&dir.join("SKILL.md")), hand);
    assert_eq!(read(&dir.join("notes.md")), "notes v2\n");

    let after = status(&store, "clash");
    assert_eq!(after["baseline"]["revision"], rev2.as_str());
    assert_eq!(changed_files(&after), vec![("SKILL.md".to_string(), "modified".to_string())]);
    assert_eq!(serde_json::to_value(merge::status(&skill).unwrap()).unwrap()["pending"], false);
}

// ── scenario 4, and scenario 5 under `--all` ──

#[test]
fn scenario_4_update_all_moves_only_the_clean_skill() {
    run_probe("scenario_4_probe");
}

#[test]
#[ignore = "run by scenario_4_update_all_moves_only_the_clean_skill"]
fn scenario_4_probe() {
    let Some(root) = probe_root() else { return };
    let store = open_library(&root);
    store.set_setting("auto_update_apply", "on").unwrap();
    let edited_up = Upstream::new(&root, "edited");
    let clean_up = Upstream::new(&root, "clean");
    let local_up = Upstream::new(&root, "with-local-file");
    let edited = install(&store, &edited_up, "edited");
    let clean = install(&store, &clean_up, "clean");
    let local = install(&store, &local_up, "with-local-file");

    let edit = format!("{}edited locally\n", read(&edited.join("SKILL.md")));
    fs::write(edited.join("SKILL.md"), &edit).unwrap();
    fs::create_dir_all(local.join("local")).unwrap();
    fs::write(local.join("local/mine.md"), "written by me\n").unwrap();
    for up in [&edited_up, &clean_up, &local_up] {
        up.commit(&[("notes.md", "notes v2\n")]);
    }
    let edited_before = snapshot(&edited);
    let local_before = snapshot(&local);

    let rows = update(&store, None);
    assert_applied_without_prompt(&rows["clean"]);
    assert_eq!(read(&clean.join("notes.md")), "notes v2\n");

    // `--all` never merges: the edited skill is held, untouched.
    let held = &rows["edited"];
    assert_eq!(held["refreshed"], false, "{held}");
    assert_eq!(held["held_back_overwrites"], serde_json::json!(["library: SKILL.md"]));
    assert_eq!(held["held_back_reason"], "local_changes");
    assert_eq!(snapshot(&edited), edited_before);
    assert_ne!(
        status(&store, "edited")["baseline"]["revision"],
        status(&store, "clean")["baseline"]["revision"],
        "the held skill keeps its old baseline"
    );

    // A local-only file is a local change too: held under `--all`, kept.
    let held = &rows["with-local-file"];
    assert_eq!(held["refreshed"], false, "{held}");
    assert_eq!(held["held_back_removals"], serde_json::json!(["library: local/"]));
    assert_eq!(snapshot(&local), local_before);

    // A named update of each then merges with no prompt (scenarios 1 and 5).
    assert_applied_without_prompt(&update(&store, Some("edited"))["edited"]);
    assert_eq!(read(&edited.join("SKILL.md")), edit);
    assert_eq!(read(&edited.join("notes.md")), "notes v2\n");
    assert_applied_without_prompt(&update(&store, Some("with-local-file"))["with-local-file"]);
    assert_eq!(read(&local.join("local/mine.md")), "written by me\n");

    // F1: with upstream where it is, a later `--all` reports nothing held.
    let rows = update(&store, None);
    for name in ["edited", "clean", "with-local-file"] {
        assert_nothing_to_do(&rows[name]);
    }
    assert!(baseline::load(&resolve_skill(&store, "edited").unwrap().id).unwrap().is_some());
}
