//! A generic runner for agent CLIs (Claude Code first).
//!
//! It knows nothing about what the agent is asked to do. A run hands the agent
//! a prompt on stdin and a JSON Schema for its answer, streams progress, and
//! ends with either the structured answer or a failure that says why.
//!
//! Every run is read-only by construction: adapters expose no way to grant
//! write or exec tools.
//!
//! The runner is store-agnostic. Settings it needs (the binary override and
//! the proxy) come in through [`RunContext`]; [`RunContext::from_store`] reads
//! them for callers that have a store.

pub mod claude_code;
pub mod codex;
pub mod resolve;

use std::fmt;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;
use serde_json::Value;

use crate::core::error::{AppError, ErrorKind};
use crate::core::process::{self, ExitInfo, ProcessError};
use crate::core::skill_store::SkillStore;
use resolve::Launch;

pub use claude_code::ClaudeCodeAdapter;

/// Default run timeout. A real evaluation run took 160 s and cost $1.02, so
/// this is deliberately generous.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(600);

/// How much of a child's stderr goes into an error message.
const STDERR_EXCERPT_CHARS: usize = 2000;

/// What to ask the agent.
#[derive(Debug, Clone)]
pub struct RunRequest {
    /// Working directory for the agent; normally an empty temporary
    /// directory, so the agent sees nothing it was not given.
    pub cwd: PathBuf,
    /// Directories the agent may read, in addition to `cwd`.
    pub extra_read_dirs: Vec<PathBuf>,
    /// Fed to the agent on stdin.
    pub prompt: String,
    /// JSON Schema the final answer must match.
    pub output_schema: Value,
    pub model: Option<String>,
    pub timeout: Duration,
    pub budget_usd: Option<f64>,
}

/// Settings a run needs from outside. Kept out of [`RunRequest`] so requests
/// can be built without a store, and out of logs: `Debug` redacts the proxy.
#[derive(Clone, Default)]
pub struct RunContext {
    /// The `agent_binary_<key>` setting, if set.
    pub binary_override: Option<String>,
    /// The app's proxy URL, passed to the child as `HTTPS_PROXY`/`HTTP_PROXY`.
    /// Never logged.
    pub proxy_url: Option<String>,
}

impl fmt::Debug for RunContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RunContext")
            .field("binary_override", &self.binary_override)
            .field("proxy_url", &self.proxy_url.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

impl RunContext {
    /// Read the binary override for `adapter_key` and the proxy from `store`.
    pub fn from_store(store: &SkillStore, adapter_key: &str) -> Self {
        Self {
            binary_override: store
                .get_setting(&resolve::setting_key(adapter_key))
                .ok()
                .flatten()
                .filter(|v| !v.trim().is_empty()),
            proxy_url: store.proxy_url(),
        }
    }
}

/// Token and cost accounting reported by the agent, when it reports any.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct RunUsage {
    pub total_cost_usd: Option<f64>,
    pub duration_ms: Option<u64>,
    pub duration_api_ms: Option<u64>,
    pub num_turns: Option<u64>,
    /// The agent's own token breakdown, passed through as reported.
    pub usage: Option<Value>,
    /// Per-model breakdown, passed through as reported.
    pub model_usage: Option<Value>,
}

/// Why a run failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureKind {
    /// The request itself is unusable (bad cwd, schema, budget...).
    InvalidRequest,
    /// The agent binary could not be found or does not exist.
    BinaryMissing,
    /// The agent could not be started, or exited before producing any result
    /// (typically rejected arguments; stderr is in the message).
    LaunchFailed,
    /// The agent is not signed in.
    Auth,
    /// The agent reported an error in its result.
    AgentError,
    /// The agent produced a result but exited non-zero.
    NonZeroExit,
    /// The agent's output had no usable result.
    InvalidOutput,
    /// The agent stopped because it reached the spend cap (`budget_usd`).
    BudgetExceeded,
    Timeout,
    Cancelled,
}

impl FailureKind {
    pub fn error_kind(self) -> ErrorKind {
        match self {
            FailureKind::Cancelled => ErrorKind::Cancelled,
            FailureKind::InvalidRequest => ErrorKind::InvalidInput,
            _ => ErrorKind::Agent,
        }
    }
}

/// What a run reports while it runs, in order: `Started`, any number of
/// `Progress`, then exactly one of `Finished` or `Failed`.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RunEvent {
    Started {
        /// What is being executed.
        command: String,
    },
    Progress {
        text: String,
    },
    Finished {
        output_json: Value,
        usage: RunUsage,
    },
    Failed {
        kind: FailureKind,
        message: String,
        /// What the agent reported spending before it stopped, when it got
        /// as far as reporting anything.
        #[serde(skip_serializing_if = "Option::is_none")]
        usage: Option<RunUsage>,
    },
}

/// A successful run's answer.
#[derive(Debug, Clone, PartialEq)]
pub struct RunOutput {
    pub output_json: Value,
    pub usage: RunUsage,
}

/// One agent CLI.
pub trait AgentAdapter: Send + Sync {
    /// Stable key, used in the `agent_binary_<key>` setting.
    fn key(&self) -> &'static str;
    /// Human-readable name, for messages.
    fn display_name(&self) -> &'static str;
    /// The binary's base name, without extension.
    fn binary_name(&self) -> &'static str;

    /// Find the binary without a settings override.
    fn detect(&self) -> Option<PathBuf> {
        resolve::resolve_binary(self.key(), self.binary_name(), None).ok()
    }

    /// The full command for `request`, starting from `launch.command()`.
    /// The runner adds the proxy, stdio, and the prompt on stdin.
    fn build_command(&self, launch: &Launch, request: &RunRequest) -> Result<Command, String>;

    /// Turn one stdout line into an event. `Finished`/`Failed` mark the
    /// agent's own verdict; the runner combines it with the exit status.
    /// Unknown or uninteresting lines give `None`.
    fn parse_line(&self, line: &str) -> Option<RunEvent>;

    /// Whether the adapter has not yet been proven against a real install.
    /// The agent picker labels such adapters "experimental".
    fn experimental(&self) -> bool {
        false
    }

    /// The line parser for one run. The runner calls this once per run and
    /// feeds it every stdout line in order. The default is the stateless
    /// [`parse_line`](Self::parse_line); adapters whose verdict spans
    /// several lines (Codex: the answer, then the usage) keep per-run state
    /// here instead of in `self`, which is shared between runs.
    fn line_parser(&self) -> Box<dyn FnMut(&str) -> Option<RunEvent> + '_> {
        Box::new(move |line| self.parse_line(line))
    }
}

#[derive(Debug)]
struct Failure {
    kind: FailureKind,
    message: String,
    usage: Option<RunUsage>,
}

fn failure(kind: FailureKind, message: impl Into<String>) -> Failure {
    Failure {
        kind,
        message: message.into(),
        usage: None,
    }
}

/// The message for a run stopped by its spend cap. The adapter cannot see the
/// request, so the runner fills in the cap.
fn budget_message(name: &str, cap: Option<f64>, usage: Option<&RunUsage>) -> String {
    let cap = match cap {
        Some(cap) => format!(" (${cap:.2}, set in Settings → Evaluation)"),
        None => String::new(),
    };
    let spent = match usage.and_then(|u| u.total_cost_usd) {
        Some(spent) => format!(" It had spent ${spent:.2}."),
        None => String::new(),
    };
    format!(
        "{name} stopped after reaching the spend cap{cap}.{spent} \
         Raise the cap or choose a cheaper model."
    )
}

/// Run `adapter` on `request`, blocking until it finishes, fails, times out or
/// `cancel` is raised. On cancel and timeout the agent's whole process tree is
/// killed.
///
/// `on_event` sees `Started`, `Progress`, and finally one `Finished` or
/// `Failed`; the return value carries the same outcome.
pub fn run(
    adapter: &dyn AgentAdapter,
    request: &RunRequest,
    context: &RunContext,
    cancel: &Arc<AtomicBool>,
    on_event: &mut dyn FnMut(RunEvent),
) -> Result<RunOutput, AppError> {
    match run_inner(adapter, request, context, cancel, on_event) {
        Ok(output) => {
            log::info!("{} run finished", adapter.display_name());
            on_event(RunEvent::Finished {
                output_json: output.output_json.clone(),
                usage: output.usage.clone(),
            });
            Ok(output)
        }
        Err(Failure {
            kind,
            message,
            usage,
        }) => {
            log::warn!("{} run failed ({kind:?})", adapter.display_name());
            on_event(RunEvent::Failed {
                kind,
                message: message.clone(),
                usage,
            });
            Err(AppError {
                kind: kind.error_kind(),
                message,
                details: None,
            })
        }
    }
}

fn run_inner(
    adapter: &dyn AgentAdapter,
    request: &RunRequest,
    context: &RunContext,
    cancel: &Arc<AtomicBool>,
    on_event: &mut dyn FnMut(RunEvent),
) -> Result<RunOutput, Failure> {
    validate(request)?;

    let binary = resolve::resolve_binary(
        adapter.key(),
        adapter.binary_name(),
        context.binary_override.as_deref(),
    )
    .map_err(|message| failure(FailureKind::BinaryMissing, message))?;
    let launch = resolve::launch_for(&binary);
    let mut cmd = adapter
        .build_command(&launch, request)
        .map_err(|message| failure(FailureKind::InvalidRequest, message))?;
    apply_proxy(&mut cmd, context.proxy_url.as_deref());

    log::info!(
        "{} run starting: {}",
        adapter.display_name(),
        launch.describe()
    );
    on_event(RunEvent::Started {
        command: launch.describe(),
    });

    // The agent's own verdict. The first failure wins: an auth error is
    // followed by a generic error result, and the auth error is the useful one.
    let mut verdict: Option<RunEvent> = None;
    let mut parse = adapter.line_parser();
    let outcome = process::run_watched(
        cmd,
        Some(request.prompt.clone().into_bytes()),
        cancel,
        request.timeout,
        |line| match parse(line) {
            Some(event @ (RunEvent::Finished { .. } | RunEvent::Failed { .. })) => {
                match (&mut verdict, event) {
                    // Keep the first failure, but take the spend the final
                    // result reports if the failure had none.
                    (
                        Some(RunEvent::Failed {
                            usage: kept @ None, ..
                        }),
                        later,
                    ) => {
                        *kept = match later {
                            RunEvent::Failed { usage, .. } => usage,
                            RunEvent::Finished { usage, .. } => Some(usage),
                            _ => None,
                        };
                    }
                    (Some(RunEvent::Failed { .. }), _) => {}
                    (_, event) => verdict = Some(event),
                }
            }
            Some(event) => on_event(event),
            None => {}
        },
    );

    let name = adapter.display_name();
    let exit = match outcome {
        Ok(exit) => exit,
        Err(ProcessError::Cancelled) => {
            return Err(failure(
                FailureKind::Cancelled,
                format!("The {name} run was cancelled."),
            ))
        }
        Err(ProcessError::TimedOut(limit)) => {
            return Err(failure(
                FailureKind::Timeout,
                format!(
                    "{name} did not finish within {}s and was stopped.",
                    limit.as_secs()
                ),
            ))
        }
        Err(ProcessError::Spawn(e)) => return Err(spawn_failure(adapter, &launch, e)),
        Err(ProcessError::Wait(e)) => {
            return Err(failure(
                FailureKind::LaunchFailed,
                format!("Lost track of the {name} process: {e}"),
            ))
        }
    };

    match verdict {
        Some(RunEvent::Failed {
            kind,
            message,
            usage,
        }) => Err(Failure {
            message: if kind == FailureKind::BudgetExceeded {
                budget_message(name, request.budget_usd, usage.as_ref())
            } else {
                message
            },
            kind,
            usage,
        }),
        Some(RunEvent::Finished { output_json, usage }) if exit.success => {
            Ok(RunOutput { output_json, usage })
        }
        Some(RunEvent::Finished { usage, .. }) => Err(Failure {
            usage: Some(usage),
            ..failure(
                FailureKind::NonZeroExit,
                format!(
                    "{name} reported a result but exited with {}.{}",
                    exit_code(&exit),
                    stderr_suffix(&exit)
                ),
            )
        }),
        _ if !exit.success => Err(failure(
            FailureKind::LaunchFailed,
            format!(
                "{name} exited with {} before producing a result.{}",
                exit_code(&exit),
                stderr_suffix(&exit)
            ),
        )),
        _ => Err(failure(
            FailureKind::InvalidOutput,
            format!("{name} exited without producing a result."),
        )),
    }
}

fn validate(request: &RunRequest) -> Result<(), Failure> {
    let invalid = |message: String| Err(failure(FailureKind::InvalidRequest, message));
    if !request.cwd.is_dir() {
        return invalid(format!(
            "The run directory {} does not exist.",
            request.cwd.display()
        ));
    }
    if let Some(dir) = request.extra_read_dirs.iter().find(|d| !d.is_dir()) {
        return invalid(format!(
            "The directory {} given to the agent does not exist.",
            dir.display()
        ));
    }
    if request.prompt.trim().is_empty() {
        return invalid("The prompt is empty.".into());
    }
    if !request.output_schema.is_object() {
        return invalid("The output schema must be a JSON object.".into());
    }
    if request.timeout.is_zero() {
        return invalid("The timeout must be greater than zero.".into());
    }
    if let Some(budget) = request.budget_usd {
        if !budget.is_finite() || budget <= 0.0 {
            return invalid(format!(
                "The budget must be a positive amount, got {budget}."
            ));
        }
    }
    Ok(())
}

/// Point the child at the app's proxy. The value is never logged.
fn apply_proxy(cmd: &mut Command, proxy_url: Option<&str>) {
    if let Some(proxy) = proxy_url.map(str::trim).filter(|p| !p.is_empty()) {
        cmd.env("HTTPS_PROXY", proxy).env("HTTP_PROXY", proxy);
    }
}

fn spawn_failure(adapter: &dyn AgentAdapter, launch: &Launch, e: std::io::Error) -> Failure {
    let name = adapter.display_name();
    let setting = resolve::setting_key(adapter.key());
    match (e.kind(), launch) {
        (std::io::ErrorKind::InvalidInput, Launch::BatchShim(shim)) => failure(
            FailureKind::LaunchFailed,
            format!(
                "Could not pass the arguments for {name} safely through the batch file {}. \
                 Set {setting} to a native executable (for example {}.exe) instead.",
                shim.display(),
                adapter.binary_name()
            ),
        ),
        (std::io::ErrorKind::NotFound, _) => failure(
            FailureKind::BinaryMissing,
            format!(
                "Could not start {name}: {} was not found ({e}).",
                launch.describe()
            ),
        ),
        _ => failure(
            FailureKind::LaunchFailed,
            format!("Could not start {name} ({}): {e}", launch.describe()),
        ),
    }
}

fn exit_code(exit: &ExitInfo) -> String {
    match exit.code {
        Some(code) => format!("code {code}"),
        None => "a signal".to_string(),
    }
}

fn stderr_suffix(exit: &ExitInfo) -> String {
    let stderr = exit.stderr.trim();
    if stderr.is_empty() {
        return String::new();
    }
    let mut excerpt: String = stderr.chars().take(STDERR_EXCERPT_CHARS).collect();
    if excerpt.len() < stderr.len() {
        excerpt.push('…');
    }
    format!(" Its error output: {excerpt}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::process::tests::shell;
    use std::sync::atomic::Ordering;
    use std::time::Instant;

    /// Runs a shell script instead of an agent, and parses its output with
    /// the real Claude parser. The "binary" is the shell, supplied through
    /// the settings override exactly as a user would.
    struct ScriptAdapter {
        script: String,
    }

    impl AgentAdapter for ScriptAdapter {
        fn key(&self) -> &'static str {
            "test_script"
        }
        fn display_name(&self) -> &'static str {
            "Test agent"
        }
        fn binary_name(&self) -> &'static str {
            "test-agent"
        }
        fn build_command(&self, _launch: &Launch, request: &RunRequest) -> Result<Command, String> {
            let mut cmd = shell(&self.script);
            cmd.current_dir(&request.cwd);
            Ok(cmd)
        }
        fn parse_line(&self, line: &str) -> Option<RunEvent> {
            ClaudeCodeAdapter.parse_line(line)
        }
    }

    fn shell_path() -> String {
        if cfg!(windows) {
            std::env::var("ComSpec").unwrap_or_else(|_| r"C:\Windows\System32\cmd.exe".into())
        } else {
            "/bin/sh".into()
        }
    }

    fn request(cwd: &std::path::Path, timeout: Duration) -> RunRequest {
        RunRequest {
            cwd: cwd.to_path_buf(),
            extra_read_dirs: vec![],
            prompt: "Evaluate the skill.".into(),
            output_schema: serde_json::json!({"type": "object"}),
            model: None,
            timeout,
            budget_usd: None,
        }
    }

    /// Write `content` to a file in `dir` and return a script that prints it
    /// to stdout (or stderr) and then exits with `code`.
    fn emit_file_script(
        dir: &std::path::Path,
        content: &str,
        to_stderr: bool,
        code: i32,
    ) -> String {
        let file = dir.join(if to_stderr { "err.txt" } else { "out.jsonl" });
        std::fs::write(&file, content).unwrap();
        if cfg!(windows) {
            let redirect = if to_stderr { " 1>&2" } else { "" };
            format!("type \"{}\"{redirect}& exit /b {code}", file.display())
        } else {
            let redirect = if to_stderr { " >&2" } else { "" };
            format!("cat '{}'{redirect}; exit {code}", file.display())
        }
    }

    fn run_script(
        script: String,
        cwd: &std::path::Path,
        timeout: Duration,
    ) -> (Result<RunOutput, AppError>, Vec<RunEvent>) {
        let adapter = ScriptAdapter { script };
        let context = RunContext {
            binary_override: Some(shell_path()),
            proxy_url: None,
        };
        let mut events = Vec::new();
        let result = run(
            &adapter,
            &request(cwd, timeout),
            &context,
            &Arc::new(AtomicBool::new(false)),
            &mut |e| events.push(e),
        );
        (result, events)
    }

    #[test]
    fn success_fixture_yields_the_structured_output() {
        let tmp = tempfile::tempdir().unwrap();
        let script = emit_file_script(
            tmp.path(),
            include_str!("testdata/claude_success.jsonl"),
            false,
            0,
        );
        let (result, events) = run_script(script, tmp.path(), Duration::from_secs(60));

        let output = result.unwrap();
        assert_eq!(output.output_json["grade"], "F");
        assert_eq!(output.output_json["score"]["value"], 57);
        assert_eq!(output.usage.total_cost_usd, Some(1.0239527));
        assert_eq!(output.usage.num_turns, Some(8));

        assert!(matches!(events.first(), Some(RunEvent::Started { .. })));
        assert!(events
            .iter()
            .any(|e| matches!(e, RunEvent::Progress { text } if text.contains("structured"))));
        assert!(matches!(events.last(), Some(RunEvent::Finished { .. })));
    }

    #[test]
    fn auth_failure_fixture_says_to_log_in() {
        let tmp = tempfile::tempdir().unwrap();
        let script = emit_file_script(
            tmp.path(),
            include_str!("testdata/claude_failure.jsonl"),
            false,
            1,
        );
        let (result, events) = run_script(script, tmp.path(), Duration::from_secs(60));

        let err = result.unwrap_err();
        assert_eq!(err.kind, ErrorKind::Agent);
        assert!(err.message.contains("claude auth login"), "{}", err.message);
        assert!(matches!(
            events.last(),
            Some(RunEvent::Failed {
                kind: FailureKind::Auth,
                ..
            })
        ));
    }

    /// SYNTHETIC: no budget-exceeded run was captured by the spike; this is a
    /// hand-written result line with the documented `error_max_budget_usd`
    /// subtype. The runner names the cap from the request and the spend.
    #[test]
    fn synthetic_budget_stop_names_the_cap_and_the_spend() {
        let tmp = tempfile::tempdir().unwrap();
        let line = r#"{"type":"result","subtype":"error_max_budget_usd","is_error":true,"result":"","total_cost_usd":0.5123,"num_turns":4}"#;
        let script = emit_file_script(tmp.path(), &format!("{line}\n"), false, 1);
        let mut req = request(tmp.path(), Duration::from_secs(60));
        req.budget_usd = Some(0.5);
        let mut events = Vec::new();
        let err = run(
            &ScriptAdapter { script },
            &req,
            &RunContext {
                binary_override: Some(shell_path()),
                proxy_url: None,
            },
            &Arc::new(AtomicBool::new(false)),
            &mut |e| events.push(e),
        )
        .unwrap_err();

        assert_eq!(err.kind, ErrorKind::Agent);
        assert_eq!(
            err.message,
            "Test agent stopped after reaching the spend cap ($0.50, set in Settings → Evaluation). \
             It had spent $0.51. Raise the cap or choose a cheaper model."
        );
        let Some(RunEvent::Failed { kind, usage, .. }) = events.last() else {
            panic!("expected Failed, got {:?}", events.last());
        };
        assert_eq!(*kind, FailureKind::BudgetExceeded);
        assert_eq!(usage.as_ref().unwrap().total_cost_usd, Some(0.5123));
    }

    #[test]
    fn auth_failure_keeps_the_spend_reported_by_the_later_result() {
        let tmp = tempfile::tempdir().unwrap();
        let script = emit_file_script(
            tmp.path(),
            include_str!("testdata/claude_failure.jsonl"),
            false,
            1,
        );
        let (_, events) = run_script(script, tmp.path(), Duration::from_secs(60));
        let Some(RunEvent::Failed { kind, usage, .. }) = events.last() else {
            panic!("expected Failed, got {:?}", events.last());
        };
        assert_eq!(*kind, FailureKind::Auth);
        assert_eq!(usage.as_ref().unwrap().total_cost_usd, Some(0.0));
    }

    #[test]
    fn is_error_result_fails_even_with_exit_zero() {
        let tmp = tempfile::tempdir().unwrap();
        // The failure fixture minus its auth line: only `is_error: true`
        // on the result says it failed.
        let lines: Vec<&str> = include_str!("testdata/claude_failure.jsonl")
            .lines()
            .filter(|l| !l.contains("authentication_failed"))
            .collect();
        let script = emit_file_script(tmp.path(), &lines.join("\n"), false, 0);
        let (result, events) = run_script(script, tmp.path(), Duration::from_secs(60));

        let err = result.unwrap_err();
        assert_eq!(err.kind, ErrorKind::Agent);
        assert!(
            err.message.contains("Failed to authenticate"),
            "{}",
            err.message
        );
        assert!(matches!(
            events.last(),
            Some(RunEvent::Failed {
                kind: FailureKind::AgentError,
                ..
            })
        ));
    }

    #[test]
    fn nonzero_exit_without_a_result_surfaces_stderr() {
        let tmp = tempfile::tempdir().unwrap();
        let script = emit_file_script(
            tmp.path(),
            include_str!("testdata/claude_bad_json_schema_arg.stderr"),
            true,
            1,
        );
        let (result, events) = run_script(script, tmp.path(), Duration::from_secs(60));

        let err = result.unwrap_err();
        assert_eq!(err.kind, ErrorKind::Agent);
        assert!(
            err.message.contains("--json-schema is not valid JSON"),
            "{}",
            err.message
        );
        assert!(matches!(
            events.last(),
            Some(RunEvent::Failed {
                kind: FailureKind::LaunchFailed,
                ..
            })
        ));
    }

    #[test]
    fn nonzero_exit_fails_even_after_a_successful_result() {
        let tmp = tempfile::tempdir().unwrap();
        let script = emit_file_script(
            tmp.path(),
            include_str!("testdata/claude_success.jsonl"),
            false,
            3,
        );
        let (result, events) = run_script(script, tmp.path(), Duration::from_secs(60));

        let err = result.unwrap_err();
        assert_eq!(err.kind, ErrorKind::Agent);
        assert!(err.message.contains("code 3"), "{}", err.message);
        assert!(matches!(
            events.last(),
            Some(RunEvent::Failed {
                kind: FailureKind::NonZeroExit,
                ..
            })
        ));
    }

    #[test]
    fn exit_zero_without_a_result_is_invalid_output() {
        let tmp = tempfile::tempdir().unwrap();
        let script = emit_file_script(tmp.path(), "not json\n{\"type\":\"user\"}\n", false, 0);
        let (result, events) = run_script(script, tmp.path(), Duration::from_secs(60));

        assert_eq!(result.unwrap_err().kind, ErrorKind::Agent);
        assert!(matches!(
            events.last(),
            Some(RunEvent::Failed {
                kind: FailureKind::InvalidOutput,
                ..
            })
        ));
    }

    #[test]
    fn a_run_past_its_timeout_fails_as_timeout() {
        let tmp = tempfile::tempdir().unwrap();
        let script = if cfg!(windows) {
            "ping -n 30 127.0.0.1 >nul".to_string()
        } else {
            "sleep 30".to_string()
        };
        let started = Instant::now();
        let (result, events) = run_script(script, tmp.path(), Duration::from_secs(1));

        assert_eq!(result.unwrap_err().kind, ErrorKind::Agent);
        assert!(matches!(
            events.last(),
            Some(RunEvent::Failed {
                kind: FailureKind::Timeout,
                ..
            })
        ));
        assert!(started.elapsed() < Duration::from_secs(20));
    }

    #[test]
    fn a_cancelled_run_reports_cancelled() {
        let tmp = tempfile::tempdir().unwrap();
        let script = if cfg!(windows) {
            "echo {\"type\":\"user\"}& ping -n 30 127.0.0.1 >nul".to_string()
        } else {
            "echo '{\"type\":\"user\"}'; sleep 30".to_string()
        };
        let adapter = ScriptAdapter { script };
        let context = RunContext {
            binary_override: Some(shell_path()),
            proxy_url: None,
        };
        let cancel = Arc::new(AtomicBool::new(false));
        let canceller = {
            let cancel = cancel.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(500));
                cancel.store(true, Ordering::SeqCst);
            })
        };
        let started = Instant::now();
        let mut events = Vec::new();
        let result = run(
            &adapter,
            &request(tmp.path(), Duration::from_secs(60)),
            &context,
            &cancel,
            &mut |e| events.push(e),
        );
        canceller.join().unwrap();

        assert_eq!(result.unwrap_err().kind, ErrorKind::Cancelled);
        assert!(matches!(
            events.last(),
            Some(RunEvent::Failed {
                kind: FailureKind::Cancelled,
                ..
            })
        ));
        assert!(started.elapsed() < Duration::from_secs(20));
    }

    #[test]
    fn a_missing_binary_is_an_agent_error_naming_the_lookup_without_hanging() {
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("nowhere").join("claude.exe");
        let context = RunContext {
            binary_override: Some(missing.display().to_string()),
            proxy_url: None,
        };
        let started = Instant::now();
        let mut events = Vec::new();
        let result = run(
            &ClaudeCodeAdapter,
            &request(tmp.path(), Duration::from_secs(60)),
            &context,
            &Arc::new(AtomicBool::new(false)),
            &mut |e| events.push(e),
        );

        let err = result.unwrap_err();
        assert_eq!(err.kind, ErrorKind::Agent);
        assert!(
            err.message.contains("agent_binary_claude_code"),
            "{}",
            err.message
        );
        assert!(
            err.message.contains(&missing.display().to_string()),
            "{}",
            err.message
        );
        assert_eq!(
            events,
            vec![RunEvent::Failed {
                kind: FailureKind::BinaryMissing,
                message: err.message.clone(),
                usage: None,
            }]
        );
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn invalid_requests_are_refused_before_spawning() {
        let tmp = tempfile::tempdir().unwrap();
        let mut req = request(tmp.path(), Duration::from_secs(60));
        req.budget_usd = Some(-1.0);
        let mut events = Vec::new();
        let err = run(
            &ScriptAdapter {
                script: "exit 0".into(),
            },
            &req,
            &RunContext {
                binary_override: Some(shell_path()),
                proxy_url: None,
            },
            &Arc::new(AtomicBool::new(false)),
            &mut |e| events.push(e),
        )
        .unwrap_err();
        assert_eq!(err.kind, ErrorKind::InvalidInput);
        assert!(!events.iter().any(|e| matches!(e, RunEvent::Started { .. })));
    }

    /// The child reports the proxy variables it received inside its result.
    #[test]
    fn the_run_passes_the_proxy_to_the_child() {
        let tmp = tempfile::tempdir().unwrap();
        let proxy = "http://proxy.invalid:8080";
        let script = if cfg!(windows) {
            r#"echo {"type":"result","is_error":false,"structured_output":{"https":"%HTTPS_PROXY%","http":"%HTTP_PROXY%"}}"#
                .to_string()
        } else {
            r#"printf '{"type":"result","is_error":false,"structured_output":{"https":"%s","http":"%s"}}\n' "$HTTPS_PROXY" "$HTTP_PROXY""#
                .to_string()
        };
        let context = RunContext {
            binary_override: Some(shell_path()),
            proxy_url: Some(proxy.into()),
        };
        let output = run(
            &ScriptAdapter { script },
            &request(tmp.path(), Duration::from_secs(60)),
            &context,
            &Arc::new(AtomicBool::new(false)),
            &mut |_| {},
        )
        .unwrap();
        assert_eq!(output.output_json["https"], proxy);
        assert_eq!(output.output_json["http"], proxy);
    }

    #[test]
    fn proxy_is_set_on_the_child_and_redacted_from_debug() {
        let proxy = "http://user:secret@proxy.invalid:8080";
        let mut cmd = Command::new("agent");
        apply_proxy(&mut cmd, Some(proxy));
        let envs: Vec<(String, Option<String>)> = cmd
            .get_envs()
            .map(|(k, v)| {
                (
                    k.to_string_lossy().into_owned(),
                    v.map(|v| v.to_string_lossy().into_owned()),
                )
            })
            .collect();
        assert!(envs.contains(&("HTTPS_PROXY".into(), Some(proxy.into()))));
        assert!(envs.contains(&("HTTP_PROXY".into(), Some(proxy.into()))));

        let context = RunContext {
            binary_override: None,
            proxy_url: Some(proxy.into()),
        };
        assert!(!format!("{context:?}").contains("secret"));
    }
}
