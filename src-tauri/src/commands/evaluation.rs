//! Tauri commands for skill evaluation.
//!
//! `start_skill_evaluation` returns a run id at once and does the work on a
//! blocking thread, reporting through `evaluation-progress` events. Each run's
//! cancel flag lives in the shared [`InstallCancelRegistry`] under its run id.
//! At most one run per skill is active; runs on different skills may overlap.
//!
//! Progress events carry only the agent's progress text, never the prompt,
//! the proxy, or the full report. The record itself is read back with
//! `get_skill_evaluations` once the final event arrives.

use std::collections::HashMap;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, OnceLock};

use serde::Serialize;
use tauri::State;

use crate::core::agent_runner::{registry, resolve, RunEvent};
use crate::core::error::{AppError, ErrorKind};
use crate::core::evaluation::{self, EvaluateOptions, EvaluationRecord, EvaluationSummary};
use crate::core::install_cancel::InstallCancelRegistry;
use crate::core::skill_store::SkillStore;

/// The event every run reports through.
pub const PROGRESS_EVENT: &str = "evaluation-progress";

/// Progress text longer than this is cut, so one noisy line cannot flood the UI.
const MAX_DETAIL_CHARS: usize = 400;

// ── Agent runners ──

/// One agent the evaluation can run on, as the pickers show it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AgentRunnerInfo {
    pub key: String,
    pub display_name: String,
    pub experimental: bool,
    /// The binary that would run, after applying the `agent_binary_<key>`
    /// setting. `None` when it cannot be found.
    pub binary: Option<String>,
    /// Why no binary was found, naming what was looked for.
    pub binary_error: Option<String>,
}

/// Every registered adapter, with its binary resolved the way a run would
/// resolve it: a bad override is reported, not replaced by a PATH match.
pub fn agent_runners(store: &SkillStore) -> Result<Vec<AgentRunnerInfo>, AppError> {
    registry::all_adapters()
        .iter()
        .map(|adapter| {
            let override_path = store
                .get_setting(&resolve::setting_key(adapter.key()))
                .map_err(AppError::db)?;
            let (binary, binary_error) = match resolve::resolve_binary(
                adapter.key(),
                adapter.binary_name(),
                override_path.as_deref(),
            ) {
                Ok(path) => (Some(path.display().to_string()), None),
                Err(message) => (None, Some(message)),
            };
            Ok(AgentRunnerInfo {
                key: adapter.key().to_string(),
                display_name: adapter.display_name().to_string(),
                experimental: adapter.experimental(),
                binary,
                binary_error,
            })
        })
        .collect()
}

// ── One run per skill ──

/// Which skill has a run in progress, and under which run id.
#[derive(Default)]
pub struct ActiveRuns {
    by_skill: Mutex<HashMap<String, String>>,
}

impl ActiveRuns {
    /// Mark `skill_id` busy with `run_id`, or refuse if a run is already
    /// active for it. The mark is cleared when the guard drops.
    pub fn claim(
        self: &Arc<Self>,
        skill_id: &str,
        run_id: &str,
    ) -> Result<ActiveRunGuard, AppError> {
        let mut map = self.by_skill.lock().unwrap_or_else(|e| e.into_inner());
        if map.contains_key(skill_id) {
            return Err(AppError::invalid_input(
                "An evaluation of this skill is already running. Wait for it to finish or cancel it.",
            ));
        }
        map.insert(skill_id.to_string(), run_id.to_string());
        Ok(ActiveRunGuard {
            runs: self.clone(),
            skill_id: skill_id.to_string(),
        })
    }

    #[cfg(test)]
    fn run_for(&self, skill_id: &str) -> Option<String> {
        self.by_skill
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(skill_id)
            .cloned()
    }
}

pub struct ActiveRunGuard {
    runs: Arc<ActiveRuns>,
    skill_id: String,
}

impl Drop for ActiveRunGuard {
    fn drop(&mut self) {
        self.runs
            .by_skill
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.skill_id);
    }
}

fn active_runs() -> &'static Arc<ActiveRuns> {
    static RUNS: OnceLock<Arc<ActiveRuns>> = OnceLock::new();
    RUNS.get_or_init(|| Arc::new(ActiveRuns::default()))
}

/// Removes a run's cancel flag from the registry when the run ends.
struct CancelGuard {
    registry: Arc<InstallCancelRegistry>,
    key: String,
}

impl Drop for CancelGuard {
    fn drop(&mut self) {
        self.registry.remove(&self.key);
    }
}

// ── Progress ──

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EvaluationPhase {
    /// The agent process has been started.
    Started,
    /// A line of the agent's own progress; `detail` holds it.
    Progress,
    /// Final: the run produced a valid result, stored as `evaluation_id`.
    Finished,
    /// Final: the run failed; `error` says why.
    Failed,
    /// Final: the user cancelled the run.
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct EvaluationProgress {
    pub run_id: String,
    pub skill_id: String,
    pub phase: EvaluationPhase,
    pub detail: Option<String>,
    /// The stored record, on a final event when the run got as far as storing one.
    pub evaluation_id: Option<String>,
    pub error: Option<String>,
}

fn truncate_chars(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        Some((cut, _)) => format!("{}…", &text[..cut]),
        None => text.to_string(),
    }
}

/// The record a failed run stored, if it got that far: the newest one for
/// the skill that started no earlier than the run did. With one run per
/// skill, that can only be this run's.
fn stored_record_since(store: &SkillStore, skill_id: &str, since_ms: i64) -> Option<String> {
    store
        .list_evaluations(skill_id)
        .ok()?
        .into_iter()
        .find(|record| record.started_at >= since_ms)
        .map(|record| record.id)
}

/// Run one evaluation and report it through `emit`, ending with exactly one
/// final event (`finished`, `failed` or `cancelled`), whatever happens,
/// a panic in the evaluation included.
///
/// `evaluate` is [`evaluation::evaluate_skill`] in the app; tests pass a stub.
pub fn run_evaluation<F>(
    store: &SkillStore,
    skill_id: &str,
    run_id: &str,
    emit: &mut dyn FnMut(EvaluationProgress),
    evaluate: F,
) -> Result<EvaluationRecord, AppError>
where
    F: FnOnce(&mut dyn FnMut(RunEvent)) -> Result<EvaluationRecord, AppError>,
{
    let started_ms = chrono::Utc::now().timestamp_millis();
    let event = |phase, detail, evaluation_id, error| EvaluationProgress {
        run_id: run_id.to_string(),
        skill_id: skill_id.to_string(),
        phase,
        detail,
        evaluation_id,
        error,
    };

    let outcome = {
        let mut forward = |run_event: RunEvent| match run_event {
            RunEvent::Started { .. } => emit(event(EvaluationPhase::Started, None, None, None)),
            RunEvent::Progress { text } => emit(event(
                EvaluationPhase::Progress,
                Some(truncate_chars(&text, MAX_DETAIL_CHARS)),
                None,
                None,
            )),
            // The final event is sent below, once the record is stored.
            RunEvent::Finished { .. } | RunEvent::Failed { .. } => {}
        };
        catch_unwind(AssertUnwindSafe(|| evaluate(&mut forward))).unwrap_or_else(|_| {
            Err(AppError::internal(
                "The evaluation stopped unexpectedly. See the log for details.",
            ))
        })
    };

    let final_event = match &outcome {
        Ok(record) => event(
            EvaluationPhase::Finished,
            None,
            Some(record.id.clone()),
            None,
        ),
        Err(err) => event(
            if err.kind == ErrorKind::Cancelled {
                EvaluationPhase::Cancelled
            } else {
                EvaluationPhase::Failed
            },
            None,
            stored_record_since(store, skill_id, started_ms),
            Some(err.message.clone()),
        ),
    };
    emit(final_event);
    outcome
}

// ── Commands ──

/// Start evaluating `skill_id`; returns the run id at once. Unset options
/// fall back to the `eval_*` settings, then the defaults.
#[tauri::command]
pub async fn start_skill_evaluation(
    skill_id: String,
    options: Option<EvaluateOptions>,
    store: State<'_, Arc<SkillStore>>,
    cancel_registry: State<'_, Arc<InstallCancelRegistry>>,
    app_handle: tauri::AppHandle,
) -> Result<String, AppError> {
    let store = store.inner().clone();
    if store
        .get_skill_by_id(&skill_id)
        .map_err(AppError::db)?
        .is_none()
    {
        return Err(AppError::not_found(format!("Skill not found: {skill_id}")));
    }

    let run_id = uuid::Uuid::new_v4().to_string();
    let active = active_runs().claim(&skill_id, &run_id)?;
    let registry = cancel_registry.inner().clone();
    let cancel: Arc<AtomicBool> = registry.register(&run_id);
    let cancel_guard = CancelGuard {
        registry,
        key: run_id.clone(),
    };
    let options = options.unwrap_or_default();
    let returned_id = run_id.clone();

    tauri::async_runtime::spawn_blocking(move || {
        use tauri::Emitter;
        let _active = active;
        let _cancel_guard = cancel_guard;
        let mut emit = |progress: EvaluationProgress| {
            if let Err(e) = app_handle.emit(PROGRESS_EVENT, &progress) {
                log::warn!("Could not emit {PROGRESS_EVENT}: {e}");
            }
        };
        let outcome = run_evaluation(&store, &skill_id, &run_id, &mut emit, |on_event| {
            evaluation::evaluate_skill(&store, &skill_id, &options, &cancel, on_event)
        });
        if let Err(err) = outcome {
            log::info!(
                "Evaluation run {run_id} of {skill_id} ended without a result: {}",
                err.message
            );
        }
    });

    Ok(returned_id)
}

/// Ask run `run_id` to stop. `false` when no such run is active.
#[tauri::command]
pub fn cancel_skill_evaluation(
    run_id: String,
    cancel_registry: State<'_, Arc<InstallCancelRegistry>>,
) -> bool {
    cancel_registry.cancel(&run_id)
}

/// Every stored run of `skill_id`, newest first.
#[tauri::command]
pub async fn get_skill_evaluations(
    skill_id: String,
    store: State<'_, Arc<SkillStore>>,
) -> Result<Vec<EvaluationRecord>, AppError> {
    let store = store.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        store.list_evaluations(&skill_id).map_err(AppError::db)
    })
    .await?
}

/// The latest successful run of every evaluated skill.
#[tauri::command]
pub async fn get_evaluation_summaries(
    store: State<'_, Arc<SkillStore>>,
) -> Result<Vec<EvaluationSummary>, AppError> {
    let store = store.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        store.latest_evaluation_summaries().map_err(AppError::db)
    })
    .await?
}

/// The agents an evaluation can run on, with their resolved binaries.
#[tauri::command]
pub async fn list_agent_runners(
    store: State<'_, Arc<SkillStore>>,
) -> Result<Vec<AgentRunnerInfo>, AppError> {
    let store = store.inner().clone();
    tauri::async_runtime::spawn_blocking(move || agent_runners(&store)).await?
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::evaluation::EvaluationStatus;
    use crate::core::skill_store::SkillRecord;
    use std::path::Path;

    fn store_in(dir: &Path) -> SkillStore {
        SkillStore::new(&dir.join("test.db")).unwrap()
    }

    fn add_skill(store: &SkillStore, id: &str) {
        store
            .insert_skill(&SkillRecord {
                id: id.into(),
                name: id.into(),
                description: None,
                source_type: "local".into(),
                source_ref: None,
                source_ref_resolved: None,
                source_subpath: None,
                source_branch: None,
                source_revision: None,
                remote_revision: None,
                central_path: format!("/library/{id}"),
                content_hash: Some("hash".into()),
                enabled: true,
                created_at: 0,
                updated_at: 0,
                status: "ok".into(),
                update_status: "local_only".into(),
                last_checked_at: None,
                last_check_error: None,
            })
            .unwrap();
    }

    fn record(skill_id: &str, status: EvaluationStatus) -> EvaluationRecord {
        let now = chrono::Utc::now().timestamp_millis();
        EvaluationRecord {
            id: uuid::Uuid::now_v7().to_string(),
            skill_id: skill_id.into(),
            skill_content_hash: Some("hash".into()),
            evaluator: "skill-judge".into(),
            evaluator_content_hash: None,
            agent: "claude_code".into(),
            model: None,
            status,
            started_at: now,
            finished_at: now,
            duration_ms: 0,
            cost_usd: None,
            result: None,
            report_md: None,
            error: None,
            stale: false,
            source_revision: None,
            baseline_revision: None,
            subject: crate::core::evaluation::EvaluationSubject::Library,
            upstream_revision: None,
        }
    }

    fn collect(
        store: &SkillStore,
        evaluate: impl FnOnce(&mut dyn FnMut(RunEvent)) -> Result<EvaluationRecord, AppError>,
    ) -> (Result<EvaluationRecord, AppError>, Vec<EvaluationProgress>) {
        let mut events = Vec::new();
        let outcome = run_evaluation(store, "skill-a", "run-1", &mut |e| events.push(e), evaluate);
        (outcome, events)
    }

    #[test]
    fn a_second_run_on_the_same_skill_is_refused_until_the_first_ends() {
        let runs = Arc::new(ActiveRuns::default());
        let first = runs.claim("skill-a", "run-1").unwrap();
        let err = runs.claim("skill-a", "run-2").err().unwrap();
        assert_eq!(err.kind, ErrorKind::InvalidInput);
        assert!(err.message.contains("already running"), "{}", err.message);
        // Another skill is independent.
        let _other = runs.claim("skill-b", "run-3").unwrap();
        assert_eq!(runs.run_for("skill-a").as_deref(), Some("run-1"));

        drop(first);
        assert_eq!(runs.run_for("skill-a"), None);
        assert!(runs.claim("skill-a", "run-4").is_ok());
    }

    #[test]
    fn agent_progress_is_forwarded_and_one_finished_event_ends_the_run() {
        let tmp = tempfile::tempdir().unwrap();
        let store = store_in(tmp.path());
        let done = record("skill-a", EvaluationStatus::Succeeded);
        let done_id = done.id.clone();
        let long = "x".repeat(MAX_DETAIL_CHARS + 50);

        let (outcome, events) = collect(&store, |on_event| {
            on_event(RunEvent::Started {
                command: "claude --secret-args".into(),
            });
            on_event(RunEvent::Progress {
                text: "Reading SKILL.md".into(),
            });
            on_event(RunEvent::Progress { text: long.clone() });
            on_event(RunEvent::Finished {
                output_json: serde_json::json!({"report_markdown": "full report"}),
                usage: Default::default(),
            });
            Ok(done)
        });

        assert!(outcome.is_ok());
        let phases: Vec<_> = events.iter().map(|e| e.phase).collect();
        assert_eq!(
            phases,
            vec![
                EvaluationPhase::Started,
                EvaluationPhase::Progress,
                EvaluationPhase::Progress,
                EvaluationPhase::Finished
            ]
        );
        assert!(events
            .iter()
            .all(|e| e.run_id == "run-1" && e.skill_id == "skill-a"));
        // The command line is not forwarded; neither is the report.
        assert_eq!(events[0].detail, None);
        assert_eq!(events[1].detail.as_deref(), Some("Reading SKILL.md"));
        assert_eq!(
            events[2].detail.as_ref().unwrap().chars().count(),
            MAX_DETAIL_CHARS + 1
        );
        let serialized = serde_json::to_string(&events).unwrap();
        assert!(!serialized.contains("full report"));
        assert!(!serialized.contains("--secret-args"));
        assert_eq!(events[3].evaluation_id.as_deref(), Some(done_id.as_str()));
        assert_eq!(events[3].error, None);
    }

    #[test]
    fn a_failed_run_ends_with_a_failed_event_naming_the_error_and_the_stored_record() {
        let tmp = tempfile::tempdir().unwrap();
        let store = store_in(tmp.path());
        add_skill(&store, "skill-a");

        let (outcome, events) = collect(&store, |on_event| {
            let failed = record("skill-a", EvaluationStatus::Failed);
            store.insert_evaluation(&failed).unwrap();
            on_event(RunEvent::Failed {
                kind: crate::core::agent_runner::FailureKind::BinaryMissing,
                message: "The setting agent_binary_claude_code points to C:\\nope".into(),
                usage: None,
            });
            Err(AppError::agent(format!(
                "Evaluation {} ended as failed: The setting agent_binary_claude_code points to C:\\nope",
                failed.id
            )))
        });

        assert!(outcome.is_err());
        assert_eq!(events.len(), 1, "{events:?}");
        let last = &events[0];
        assert_eq!(last.phase, EvaluationPhase::Failed);
        assert!(last
            .error
            .as_deref()
            .unwrap()
            .contains("agent_binary_claude_code"));
        let stored = store.list_evaluations("skill-a").unwrap();
        assert_eq!(last.evaluation_id.as_deref(), Some(stored[0].id.as_str()));
    }

    #[test]
    fn a_failure_before_any_record_carries_no_record_id() {
        let tmp = tempfile::tempdir().unwrap();
        let store = store_in(tmp.path());
        add_skill(&store, "skill-a");
        // An older run must not be mistaken for this one.
        let mut old = record("skill-a", EvaluationStatus::Succeeded);
        old.started_at -= 60_000;
        store.insert_evaluation(&old).unwrap();

        let (_, events) = collect(&store, |_| {
            Err(AppError::not_found(
                "The evaluator skill \"skill-judge\" is not in the library.",
            ))
        });

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].phase, EvaluationPhase::Failed);
        assert_eq!(events[0].evaluation_id, None);
    }

    #[test]
    fn a_cancelled_run_ends_with_a_cancelled_event() {
        let tmp = tempfile::tempdir().unwrap();
        let store = store_in(tmp.path());
        let (_, events) = collect(&store, |_| {
            Err(AppError::cancelled("Cancelled by the user"))
        });
        assert_eq!(events.last().unwrap().phase, EvaluationPhase::Cancelled);
    }

    #[test]
    fn a_panicking_evaluation_still_sends_a_final_event() {
        let tmp = tempfile::tempdir().unwrap();
        let store = store_in(tmp.path());
        let (outcome, events) = collect(&store, |_| panic!("boom"));
        assert_eq!(outcome.unwrap_err().kind, ErrorKind::Internal);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].phase, EvaluationPhase::Failed);
    }

    #[test]
    fn progress_events_serialize_with_snake_case_fields_and_phases() {
        let value = serde_json::to_value(EvaluationProgress {
            run_id: "r".into(),
            skill_id: "s".into(),
            phase: EvaluationPhase::Cancelled,
            detail: None,
            evaluation_id: Some("e".into()),
            error: Some("x".into()),
        })
        .unwrap();
        assert_eq!(
            value,
            serde_json::json!({"run_id": "r", "skill_id": "s", "phase": "cancelled",
                               "detail": null, "evaluation_id": "e", "error": "x"})
        );
    }

    #[test]
    fn runners_list_every_adapter_and_report_a_bad_binary_override() {
        let tmp = tempfile::tempdir().unwrap();
        let store = store_in(tmp.path());
        let missing = tmp.path().join("no-such-claude.exe");
        store
            .set_setting("agent_binary_claude_code", &missing.display().to_string())
            .unwrap();
        let real = tmp.path().join("codex.exe");
        std::fs::write(&real, b"").unwrap();
        store
            .set_setting("agent_binary_codex", &real.display().to_string())
            .unwrap();

        let runners = agent_runners(&store).unwrap();

        let keys: Vec<_> = runners.iter().map(|r| r.key.as_str()).collect();
        assert_eq!(keys, vec!["claude_code", "codex"]);
        let claude = &runners[0];
        assert!(!claude.experimental);
        assert_eq!(claude.binary, None);
        let why = claude.binary_error.as_deref().unwrap();
        assert!(why.contains("agent_binary_claude_code"), "{why}");
        assert!(why.contains(&missing.display().to_string()), "{why}");
        let codex = &runners[1];
        assert!(codex.experimental);
        assert_eq!(
            codex.binary.as_deref(),
            Some(real.display().to_string().as_str())
        );
        assert_eq!(codex.binary_error, None);
    }
}
