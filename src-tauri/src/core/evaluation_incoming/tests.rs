//! `skills evaluate --incoming` end to end: install from a local git
//! repository, change the library copy and upstream, evaluate the incoming
//! version with a script standing in for the agent, and check that the
//! library, its git status and the skill's row are as they were. No network.
//!
//! The update path refuses `file://` URLs, so the work runs in a child test
//! process whose git maps a made-up https host onto a local directory,
//! through `GIT_CONFIG_*` in the child's environment only (the arrangement
//! the merge, baseline and pinning tests use).

use super::*;
use crate::commands::skills as cmd;
use crate::core::agent_runner::resolve::Launch;
use crate::core::agent_runner::{self, AgentAdapter, RunRequest};
use crate::core::evaluation::{EvaluationStatus, ResolvedOptions};
use crate::core::process::tests::shell;
use crate::core::repo_lock::RepoLock;
use std::collections::BTreeMap;
use std::fs;
use std::process::Command;
use std::sync::Mutex;
use std::time::Duration;
use tempfile::tempdir;

const PROBE_ENV: &str = "SKILLS_MANAGER_INCOMING_EVAL_PROBE";
const FAKE_HOST: &str = "https://incoming-eval-test.invalid/";

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

fn file_url(path: &Path) -> String {
    let raw = path.display().to_string().replace('\\', "/");
    if raw.starts_with('/') {
        format!("file://{raw}")
    } else {
        format!("file:///{raw}")
    }
}

fn run_probe(probe: &str) {
    let root = tempdir().unwrap();
    let name = format!("{}::{probe}", module_path!().split_once("::").unwrap().1);
    let output = Command::new(std::env::current_exe().unwrap())
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

fn probe_root() -> Option<PathBuf> {
    std::env::var_os(PROBE_ENV).map(PathBuf::from)
}

/// Every file under `dir` with its bytes.
fn snapshot(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    walkdir::WalkDir::new(dir)
        .into_iter()
        .flatten()
        .filter(|e| e.file_type().is_file())
        .filter(|e| !e.path().components().any(|c| c.as_os_str() == ".git"))
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

// ── A script standing in for the agent ──

/// Prints a canned stream-json file and records the request it was given.
struct FakeAgent {
    script: String,
    seen: Mutex<Option<RunRequest>>,
    /// What the target directory held while the agent ran.
    target_files: Mutex<BTreeMap<String, Vec<u8>>>,
}

impl AgentAdapter for FakeAgent {
    fn key(&self) -> &'static str {
        "fake_agent"
    }
    fn display_name(&self) -> &'static str {
        "Fake agent"
    }
    fn binary_name(&self) -> &'static str {
        "fake-agent"
    }
    fn build_command(&self, _launch: &Launch, request: &RunRequest) -> Result<Command, String> {
        *self.seen.lock().unwrap() = Some(request.clone());
        *self.target_files.lock().unwrap() = snapshot(&request.extra_read_dirs[0]);
        let mut cmd = shell(&self.script);
        cmd.current_dir(&request.cwd);
        Ok(cmd)
    }
    fn parse_line(&self, line: &str) -> Option<RunEvent> {
        agent_runner::ClaudeCodeAdapter.parse_line(line)
    }
}

fn shell_path() -> String {
    if cfg!(windows) {
        std::env::var("ComSpec").unwrap_or_else(|_| r"C:\Windows\System32\cmd.exe".into())
    } else {
        "/bin/sh".into()
    }
}

fn fake_agent(dir: &Path, grade: &str) -> FakeAgent {
    let answer = serde_json::json!({
        "schema_version": 1,
        "verdict": format!("Grade {grade}."),
        "grade": grade,
        "score": {"value": 100, "max": 120},
        "report_markdown": "# Report"
    });
    let line = serde_json::json!({"type": "result", "is_error": false,
        "total_cost_usd": 0.1, "structured_output": answer});
    let file = dir.join(format!("out-{grade}.jsonl"));
    fs::write(&file, format!("{line}\n")).unwrap();
    let script = if cfg!(windows) {
        format!("type \"{}\"& exit /b 0", file.display())
    } else {
        format!("cat '{}'; exit 0", file.display())
    };
    FakeAgent {
        script,
        seen: Mutex::new(None),
        target_files: Mutex::new(BTreeMap::new()),
    }
}

fn options() -> ResolvedOptions {
    ResolvedOptions {
        agent: "fake_agent".into(),
        evaluator: "skill-judge".into(),
        model: None,
        timeout: Duration::from_secs(60),
        budget_usd: Some(2.0),
    }
}

fn context() -> RunContext {
    RunContext {
        binary_override: Some(shell_path()),
        proxy_url: None,
    }
}

// ── Tests ──

#[test]
fn incoming_evaluation_judges_the_update_and_leaves_the_library_unchanged() {
    run_probe("probe_incoming_evaluation");
}

#[test]
#[ignore = "run by incoming_evaluation_judges_the_update_and_leaves_the_library_unchanged"]
fn probe_incoming_evaluation() {
    let Some(root) = probe_root() else { return };
    let _lock = central_repo::test_base_dir_lock();
    let base = root.join("base");
    central_repo::set_test_base_dir_override(Some(base.clone()));
    let skills_dir = central_repo::skills_dir();
    fs::create_dir_all(&skills_dir).unwrap();
    let store = SkillStore::new(&base.join("test.db")).unwrap();

    // Upstream, installed at its first revision.
    let up = root.join("demo-up");
    write(&up, "SKILL.md", b"---\nname: demo\n---\nA demo skill.\n");
    write(&up, "notes.md", b"notes v1\n");
    git_in(&up, &["init", "-q", "-b", "main"]);
    git_in(&up, &["config", "uploadpack.allowFilter", "true"]);
    git_in(&up, &["add", "-A"]);
    git_in(&up, &["commit", "-qm", "init"]);
    let installed_rev = git_in(&up, &["rev-parse", "HEAD"]);
    let url = format!("{FAKE_HOST}demo-up");
    let checkout = git_fetcher::clone_repo_ref_scoped(&url, None, None, None, None, None).unwrap();
    git_fetcher::checkout_revision(&checkout, &installed_rev).unwrap();
    let library = skills_dir.join("demo");
    let installed =
        installer::install_skill_dir_to_destination(&checkout, "demo", &library).unwrap();
    git_fetcher::cleanup_temp(&checkout);
    let meta = cmd::InstallSourceMetadata {
        source_type: "git".to_string(),
        source_ref: Some(url.clone()),
        source_ref_resolved: Some(url.clone()),
        source_subpath: None,
        source_branch: None,
        source_revision: Some(installed_rev.clone()),
        remote_revision: Some(installed_rev.clone()),
        update_status: "up_to_date".to_string(),
    };
    let skill_id = {
        let _lock = RepoLock::acquire_foreground("test").unwrap();
        cmd::store_installed_skill_unlocked(&store, &installed, &meta, None).unwrap()
    };
    assert_eq!(
        evaluation::current_baseline_revision(&skill_id).as_deref(),
        Some(installed_rev.as_str()),
        "the install recorded a baseline"
    );

    // The evaluator, a local library skill.
    let judge = skills_dir.join("skill-judge");
    write(&judge, "SKILL.md", b"---\nname: skill-judge\n---\nJudge.\n");
    store
        .insert_skill(&SkillRecord {
            id: "judge".into(),
            name: "skill-judge".into(),
            description: None,
            source_type: "local".into(),
            source_ref: None,
            source_ref_resolved: None,
            source_subpath: None,
            source_branch: None,
            source_revision: None,
            remote_revision: None,
            central_path: judge.display().to_string(),
            content_hash: Some(content_hash::hash_directory(&judge).unwrap()),
            enabled: true,
            created_at: 0,
            updated_at: 0,
            status: "ok".into(),
            update_status: "local_only".into(),
            last_checked_at: None,
            last_check_error: None,
        })
        .unwrap();

    // A local edit in the library, and a new upstream version.
    let skill_md = library.join("SKILL.md");
    let mut edited = fs::read(&skill_md).unwrap();
    edited.extend_from_slice(b"My local note.\n");
    fs::write(&skill_md, &edited).unwrap();
    write(&up, "notes.md", b"notes v2\n");
    git_in(&up, &["add", "-A"]);
    git_in(&up, &["commit", "-qm", "v2"]);
    let incoming_rev = git_in(&up, &["rev-parse", "HEAD"]);

    // The library is a git repository with everything committed.
    git_in(&skills_dir, &["init", "-q", "-b", "main"]);
    git_in(&skills_dir, &["add", "-A"]);
    git_in(&skills_dir, &["commit", "-qm", "library"]);
    assert_eq!(git_in(&skills_dir, &["status", "--porcelain"]), "");

    let skill = store.get_skill_by_id(&skill_id).unwrap().unwrap();
    let library_before = snapshot(&skills_dir);
    let work_root = root.join("work");
    let stage_root = root.join("stage");
    let scripts = root.join("scripts");
    fs::create_dir_all(&scripts).unwrap();

    // A run of the library copy first, to compare with.
    let library_agent = fake_agent(&scripts, "C");
    let library_run = evaluation::evaluate_with(
        &store,
        &skill,
        None,
        &options(),
        &library_agent,
        &context(),
        &work_root,
        &Arc::new(AtomicBool::new(false)),
        &mut |_| {},
    )
    .unwrap();
    assert_eq!(library_run.subject, EvaluationSubject::Library);
    assert_eq!(
        library_run.source_revision.as_deref(),
        Some(installed_rev.as_str())
    );
    assert_eq!(
        library_run.baseline_revision.as_deref(),
        Some(installed_rev.as_str())
    );

    // The incoming version.
    let agent = fake_agent(&scripts, "A");
    let mut progress = Vec::new();
    let outcome = evaluate_incoming_with(
        &store,
        &skill,
        &stage_root,
        None,
        &Arc::new(AtomicBool::new(false)),
        &mut |event| {
            if let RunEvent::Progress { text } = event {
                progress.push(text);
            }
        },
        |target, on_event| {
            assert!(
                !target.dir.starts_with(&skills_dir),
                "staged outside the library"
            );
            assert!(target.dir.starts_with(&stage_root));
            evaluation::evaluate_with(
                &store,
                &skill,
                Some(target),
                &options(),
                &agent,
                &context(),
                &work_root,
                &Arc::new(AtomicBool::new(false)),
                on_event,
            )
        },
    )
    .unwrap();

    // The agent read the merged tree an update would install: upstream's
    // new notes and the local edit.
    let seen = agent.target_files.lock().unwrap().clone();
    assert_eq!(
        seen.get("notes.md").map(Vec::as_slice),
        Some(&b"notes v2\n"[..])
    );
    assert_eq!(seen.get("SKILL.md"), Some(&edited));
    let request = agent.seen.lock().unwrap().clone().unwrap();
    assert!(request
        .prompt
        .contains(&request.extra_read_dirs[0].display().to_string()));
    assert!(
        progress.iter().any(|p| p.contains(&incoming_rev[..12])),
        "{progress:?}"
    );

    // The v1 result, tied to the incoming revision.
    let run = &outcome.evaluation;
    assert_eq!(run.status, EvaluationStatus::Succeeded);
    assert_eq!(run.result.as_ref().unwrap().schema_version, 1);
    assert_eq!(run.result.as_ref().unwrap().grade.as_deref(), Some("A"));
    assert_eq!(run.subject, EvaluationSubject::Incoming);
    assert_eq!(run.source_revision.as_deref(), Some(incoming_rev.as_str()));
    assert_eq!(
        run.baseline_revision.as_deref(),
        Some(installed_rev.as_str())
    );
    assert!(run.stale, "the library does not hold the incoming tree yet");
    assert_eq!(outcome.incoming.revision, incoming_rev);
    assert_eq!(
        outcome.incoming.installed_revision.as_deref(),
        Some(installed_rev.as_str())
    );
    assert!(outcome.incoming.merged_local_changes);
    assert!(outcome
        .incoming
        .kept_local
        .contains(&"SKILL.md".to_string()));
    assert!(outcome.incoming.conflicts.is_empty());
    assert!(!outcome.incoming.same_as_library);
    assert_eq!(
        outcome.latest_library.as_ref().map(|r| r.id.as_str()),
        Some(library_run.id.as_str())
    );
    let json = serde_json::to_value(&outcome).unwrap();
    assert_eq!(json["subject"], serde_json::json!("incoming"));
    assert_eq!(json["source_revision"], serde_json::json!(incoming_rev));
    assert_eq!(json["result"]["schema_version"], serde_json::json!(1));
    assert_eq!(
        json["latest_library"]["id"],
        serde_json::json!(library_run.id)
    );

    // Nothing left behind, nothing changed.
    assert_eq!(
        fs::read_dir(&stage_root).unwrap().count(),
        0,
        "staged tree removed"
    );
    assert_eq!(snapshot(&skills_dir), library_before);
    assert_eq!(git_in(&skills_dir, &["status", "--porcelain"]), "");
    let after = store.get_skill_by_id(&skill_id).unwrap().unwrap();
    assert_eq!(after.source_revision, skill.source_revision);
    assert_eq!(after.update_status, skill.update_status);
    assert_eq!(after.content_hash, skill.content_hash);
    assert_eq!(
        evaluation::current_baseline_revision(&skill_id).as_deref(),
        Some(installed_rev.as_str())
    );
    // The card badge still stands for the library copy.
    let summaries = store.latest_evaluation_summaries().unwrap();
    assert_eq!(summaries.len(), 1);
    assert_eq!(summaries[0].evaluation_id, library_run.id);

    // After the update, the library holds what the incoming run judged.
    let applied = cmd::update_git_skill_with(
        &store,
        &skill_id,
        None,
        None,
        None,
        skill_update_merge::MergeMode::WhenLocallyChanged,
    )
    .unwrap();
    assert!(applied.content_changed);
    let incoming_row = store.get_evaluation(&run.id).unwrap().unwrap();
    assert!(!incoming_row.stale, "the applied update is what was judged");
    assert_eq!(
        incoming_row.upstream_revision.as_deref(),
        Some(incoming_rev.as_str())
    );

    central_repo::set_test_base_dir_override(None);
}

#[test]
fn a_skill_without_an_upstream_has_no_incoming_version() {
    let tmp = tempdir().unwrap();
    let store = SkillStore::new(&tmp.path().join("test.db")).unwrap();
    let skill = SkillRecord {
        id: "local".into(),
        name: "local".into(),
        description: None,
        source_type: "local".into(),
        source_ref: None,
        source_ref_resolved: None,
        source_subpath: None,
        source_branch: None,
        source_revision: None,
        remote_revision: None,
        central_path: tmp.path().join("local").display().to_string(),
        content_hash: None,
        enabled: true,
        created_at: 0,
        updated_at: 0,
        status: "ok".into(),
        update_status: "local_only".into(),
        last_checked_at: None,
        last_check_error: None,
    };
    store.insert_skill(&skill).unwrap();
    let stage_root = tmp.path().join("stage");
    let mut ran = false;

    let err = evaluate_incoming_with(
        &store,
        &skill,
        &stage_root,
        None,
        &Arc::new(AtomicBool::new(false)),
        &mut |_| {},
        |_, _| {
            ran = true;
            unreachable!()
        },
    )
    .unwrap_err();

    assert_eq!(err.kind, crate::core::error::ErrorKind::InvalidInput);
    assert!(err.message.contains("no upstream"), "{}", err.message);
    assert!(!ran);
    assert!(!stage_root.exists());
    let audit = store.list_audit(None).unwrap();
    assert_eq!(audit.len(), 1);
    assert_eq!(audit[0].action, "evaluate");
    assert!(!audit[0].success);
}
