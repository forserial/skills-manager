//! The Claude Code adapter: `claude -p` with `stream-json` output.
//!
//! What the spike (sm-9dn.1, Claude Code 2.1.317) established, and this code
//! relies on:
//!
//! - `--json-schema` takes inline JSON only, so the schema goes in as one
//!   minified argument.
//! - Failure is `is_error: true` on the `result` event, a non-zero exit, or no
//!   `result` event at all. `subtype` is "success" even on failure, so it is
//!   never consulted; `stop_reason` "tool_use" is normal on success.
//! - Event order is not fixed (hooks can precede `system/init`;
//!   `rate_limit_event` and `system/thinking_tokens` are interleaved), so
//!   events are matched by type, never by position, and unknown ones ignored.
//! - The answer is `structured_output` on the final `result` event.
//! - A synthetic assistant event with `"error": "authentication_failed"`
//!   means the CLI is not signed in.

use std::ffi::{OsStr, OsString};
use std::process::Command;

use serde_json::Value;

use super::resolve::Launch;
use super::{AgentAdapter, FailureKind, RunEvent, RunRequest, RunUsage};

/// The only tools the agent gets. Read-only by construction.
const READ_ONLY_TOOLS: &str = "Read,Glob,Grep";

const AUTH_MESSAGE: &str = "Claude Code is not signed in (authentication failed). \
     Run `claude auth login` in a terminal, then try again.";

/// Longest argument shown in a progress line.
const PROGRESS_ARG_CHARS: usize = 60;

/// Longest agent-reported error message passed on.
const ERROR_MESSAGE_CHARS: usize = 1000;

pub struct ClaudeCodeAdapter;

impl AgentAdapter for ClaudeCodeAdapter {
    fn key(&self) -> &'static str {
        "claude_code"
    }

    fn display_name(&self) -> &'static str {
        "Claude Code"
    }

    fn binary_name(&self) -> &'static str {
        "claude"
    }

    fn build_command(&self, launch: &Launch, request: &RunRequest) -> Result<Command, String> {
        let mut cmd = launch.command();
        cmd.args(build_args(request)?);
        cmd.current_dir(&request.cwd);
        scrub_env(&mut cmd);
        Ok(cmd)
    }

    fn parse_line(&self, line: &str) -> Option<RunEvent> {
        parse_line(line)
    }
}

/// The arguments after the program (and after the entry script for a node
/// launch). The prompt is not among them: it goes on stdin.
pub fn build_args(request: &RunRequest) -> Result<Vec<OsString>, String> {
    let schema = serde_json::to_string(&request.output_schema)
        .map_err(|e| format!("The output schema cannot be serialized: {e}"))?;
    let mut args: Vec<OsString> = [
        "-p",
        "--output-format",
        "stream-json",
        "--verbose",
        "--restricted",
        "--tools",
        READ_ONLY_TOOLS,
        "--permission-prompts",
        "none",
        "--strict-mcp-config",
        "--no-session-persistence",
    ]
    .iter()
    .map(OsString::from)
    .collect();
    for dir in &request.extra_read_dirs {
        args.push("--add-dir".into());
        args.push(dir.as_os_str().to_owned());
    }
    args.push("--json-schema".into());
    args.push(schema.into());
    if let Some(model) = request
        .model
        .as_deref()
        .map(str::trim)
        .filter(|m| !m.is_empty())
    {
        args.push("--model".into());
        args.push(model.into());
    }
    if let Some(budget) = request.budget_usd {
        args.push("--max-budget-usd".into());
        args.push(budget.to_string().into());
    }
    Ok(args)
}

/// Whether an inherited variable must not reach the child. These make a
/// nested `claude` believe it runs inside another Claude Code session.
/// Credentials and config (`ANTHROPIC_API_KEY`, `ANTHROPIC_BASE_URL`,
/// `CLAUDE_CONFIG_DIR`) are kept.
fn is_scrubbed(key: &OsStr) -> bool {
    // Windows environment names are case-insensitive.
    let key = key.to_string_lossy().to_ascii_uppercase();
    key == "CLAUDECODE"
        || key == "CLAUDE_PID"
        || key == "CLAUDE_EFFORT"
        || key.starts_with("CLAUDE_CODE_")
        || key.starts_with("CLAUDE_AGENT_SDK_")
}

/// Remove the scrubbed variables, whether inherited from this process or set
/// on `cmd` explicitly.
pub fn scrub_env(cmd: &mut Command) {
    let mut keys: Vec<OsString> = std::env::vars_os()
        .map(|(k, _)| k)
        .chain(cmd.get_envs().map(|(k, _)| k.to_owned()))
        .filter(|k| is_scrubbed(k))
        .collect();
    keys.sort();
    keys.dedup();
    for key in keys {
        cmd.env_remove(key);
    }
}

/// One `stream-json` line to an event. See the module docs for the rules.
pub fn parse_line(line: &str) -> Option<RunEvent> {
    let event: Value = serde_json::from_str(line.trim()).ok()?;
    match event.get("type")?.as_str()? {
        "system" => {
            if event.get("subtype").and_then(Value::as_str) != Some("init") {
                return None;
            }
            let text = match event.get("model").and_then(Value::as_str) {
                Some(model) => format!("Session started ({model})"),
                None => "Session started".to_string(),
            };
            Some(RunEvent::Progress { text })
        }
        "assistant" => {
            if event.get("error").and_then(Value::as_str) == Some("authentication_failed") {
                return Some(RunEvent::Failed {
                    kind: FailureKind::Auth,
                    message: AUTH_MESSAGE.to_string(),
                    usage: None,
                });
            }
            let steps: Vec<String> = event
                .pointer("/message/content")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter(|block| block.get("type").and_then(Value::as_str) == Some("tool_use"))
                .map(describe_tool_use)
                .collect();
            if steps.is_empty() {
                None
            } else {
                Some(RunEvent::Progress {
                    text: steps.join("; "),
                })
            }
        }
        "result" => Some(parse_result(&event)),
        // user/tool results, rate_limit_event, hook_*, stream events, and
        // anything newer: not ours to interpret.
        _ => None,
    }
}

/// A limit the CLI stopped on, read from `subtype` / `terminal_reason`.
///
/// `subtype` is never trusted to mean success ("success" is reported on
/// failures too), but an `error_*` subtype names the cause well.
#[derive(Debug, PartialEq, Eq)]
enum LimitStop {
    Budget,
    MaxTurns,
}

fn limit_stop(event: &Value) -> Option<LimitStop> {
    let fields: Vec<String> = ["subtype", "terminal_reason"]
        .iter()
        .filter_map(|key| event.get(*key).and_then(Value::as_str))
        .map(str::to_ascii_lowercase)
        .collect();
    if fields.iter().any(|f| f.contains("budget")) {
        Some(LimitStop::Budget)
    } else if fields.iter().any(|f| f.contains("max_turns")) {
        Some(LimitStop::MaxTurns)
    } else {
        None
    }
}

/// "(subtype error_during_execution, terminal reason api_error)", leaving out
/// a "success" subtype, which says nothing on a failure.
fn reason_suffix(event: &Value) -> String {
    let mut parts = Vec::new();
    if let Some(subtype) = event.get("subtype").and_then(Value::as_str) {
        if subtype != "success" && !subtype.is_empty() {
            parts.push(format!("subtype {subtype}"));
        }
    }
    if let Some(reason) = event.get("terminal_reason").and_then(Value::as_str) {
        if !reason.is_empty() {
            parts.push(format!("terminal reason {reason}"));
        }
    }
    if parts.is_empty() {
        String::new()
    } else {
        format!(" ({})", parts.join(", "))
    }
}

fn parse_result(event: &Value) -> RunEvent {
    let usage = Some(usage_of(event));
    let is_error = event.get("is_error").and_then(Value::as_bool) == Some(true);
    let structured = match event.get("structured_output") {
        Some(output @ Value::Object(_)) if !is_error => Some(output.clone()),
        _ => None,
    };
    if let Some(output_json) = structured {
        return RunEvent::Finished {
            output_json,
            usage: usage.unwrap_or_default(),
        };
    }

    // A failure. A limit stop is named whether or not is_error is set: a run
    // cut off by a limit can end without a structured result either way.
    match limit_stop(event) {
        Some(LimitStop::Budget) => {
            return RunEvent::Failed {
                kind: FailureKind::BudgetExceeded,
                // The runner rewrites this with the cap from the request.
                message: "Claude Code stopped after reaching the spend cap.".to_string(),
                usage,
            };
        }
        Some(LimitStop::MaxTurns) => {
            return RunEvent::Failed {
                kind: FailureKind::AgentError,
                message: format!(
                    "Claude Code stopped after reaching its turn limit{} without finishing.",
                    event
                        .get("num_turns")
                        .and_then(Value::as_u64)
                        .map(|n| format!(" ({n} turns)"))
                        .unwrap_or_default()
                ),
                usage,
            }
        }
        None => {}
    }

    if is_error {
        let message = match event.get("result").and_then(Value::as_str).map(str::trim) {
            Some(text) if !text.is_empty() => truncate(text, ERROR_MESSAGE_CHARS),
            _ => format!("Claude Code reported an error{}.", reason_suffix(event)),
        };
        return RunEvent::Failed {
            kind: FailureKind::AgentError,
            message,
            usage,
        };
    }
    RunEvent::Failed {
        kind: FailureKind::InvalidOutput,
        message: format!(
            "Claude Code finished without a structured result{}.",
            reason_suffix(event)
        ),
        usage,
    }
}

fn usage_of(event: &Value) -> RunUsage {
    RunUsage {
        total_cost_usd: event.get("total_cost_usd").and_then(Value::as_f64),
        duration_ms: event.get("duration_ms").and_then(Value::as_u64),
        duration_api_ms: event.get("duration_api_ms").and_then(Value::as_u64),
        num_turns: event.get("num_turns").and_then(Value::as_u64),
        usage: event.get("usage").cloned(),
        model_usage: event.get("modelUsage").cloned(),
    }
}

/// A short line for a tool call, e.g. "Read SKILL.md". Never the content.
fn describe_tool_use(block: &Value) -> String {
    let name = block.get("name").and_then(Value::as_str).unwrap_or("tool");
    let input = |key: &str| {
        block
            .pointer(&format!("/input/{key}"))
            .and_then(Value::as_str)
    };
    match name {
        "Read" => match input("file_path") {
            Some(path) => {
                let file = path.rsplit(['/', '\\']).next().unwrap_or(path);
                format!("Read {}", truncate(file, PROGRESS_ARG_CHARS))
            }
            None => "Read".to_string(),
        },
        "Glob" | "Grep" => match input("pattern") {
            Some(pattern) => format!("{name} {}", truncate(pattern, PROGRESS_ARG_CHARS)),
            None => name.to_string(),
        },
        "StructuredOutput" => "Writing the structured result".to_string(),
        other => truncate(other, PROGRESS_ARG_CHARS),
    }
}

fn truncate(text: &str, max_chars: usize) -> String {
    let mut chars = text.chars();
    let mut out: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        out.push('…');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::agent_runner::resolve;
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    const SUCCESS: &str = include_str!("testdata/claude_success.jsonl");
    const FAILURE: &str = include_str!("testdata/claude_failure.jsonl");

    fn parse_all(fixture: &str) -> Vec<RunEvent> {
        fixture.lines().filter_map(parse_line).collect()
    }

    #[test]
    fn success_fixture_parses_to_progress_then_finished() {
        let events = parse_all(SUCCESS);
        assert_eq!(
            events[..2],
            [
                RunEvent::Progress {
                    text: "Session started (claude-fable-5-1)".into()
                },
                RunEvent::Progress {
                    text: "Writing the structured result".into()
                },
            ]
        );
        let RunEvent::Finished { output_json, usage } = &events[2] else {
            panic!("expected Finished, got {:?}", events[2]);
        };
        assert_eq!(events.len(), 3, "the user tool_result line yields nothing");
        // structured_output, not the `result` string copy.
        assert!(output_json.is_object());
        assert_eq!(output_json["schema_version"], 1);
        assert_eq!(output_json["grade"], "F");
        assert_eq!(usage.total_cost_usd, Some(1.0239527));
        assert_eq!(usage.duration_api_ms, Some(160410));
        assert_eq!(usage.num_turns, Some(8));
        assert!(usage.usage.is_some() && usage.model_usage.is_some());
    }

    #[test]
    fn failure_fixture_parses_to_auth_then_agent_error() {
        let events = parse_all(FAILURE);
        assert_eq!(events.len(), 3);
        assert_eq!(
            events[1],
            RunEvent::Failed {
                kind: FailureKind::Auth,
                message: AUTH_MESSAGE.into(),
                usage: None,
            }
        );
        // The result says subtype "success"; is_error is what counts.
        let RunEvent::Failed {
            kind,
            message,
            usage,
        } = &events[2]
        else {
            panic!("expected Failed, got {:?}", events[2]);
        };
        assert_eq!(*kind, FailureKind::AgentError);
        assert_eq!(
            message,
            "Failed to authenticate: OAuth session expired and could not be refreshed"
        );
        assert_eq!(usage.as_ref().unwrap().total_cost_usd, Some(0.0));
    }

    #[test]
    fn result_without_structured_output_is_invalid_output() {
        let line = r#"{"type":"result","subtype":"success","is_error":false,"result":"{\"a\":1}","terminal_reason":"completed"}"#;
        let Some(RunEvent::Failed { kind, message, .. }) = parse_line(line) else {
            panic!("expected Failed");
        };
        assert_eq!(kind, FailureKind::InvalidOutput);
        // The terminal reason is named; a "success" subtype is not.
        assert_eq!(
            message,
            "Claude Code finished without a structured result (terminal reason completed)."
        );
    }

    // SYNTHETIC: no budget-exceeded or max-turns run was captured by the
    // spike. These result lines are hand-written from the documented
    // `error_max_budget_usd` / `error_max_turns` subtypes, with the fields a
    // real result event carries.

    fn failed(line: &str) -> (FailureKind, String, Option<RunUsage>) {
        match parse_line(line) {
            Some(RunEvent::Failed {
                kind,
                message,
                usage,
            }) => (kind, message, usage),
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[test]
    fn synthetic_budget_stop_with_is_error_true_is_budget_exceeded() {
        let line = r#"{"type":"result","subtype":"error_max_budget_usd","is_error":true,"result":"","terminal_reason":"max_budget_usd","total_cost_usd":0.5123,"num_turns":4,"duration_ms":61000}"#;
        let (kind, message, usage) = failed(line);
        assert_eq!(kind, FailureKind::BudgetExceeded);
        assert!(message.contains("spend cap"), "{message}");
        assert_eq!(usage.unwrap().total_cost_usd, Some(0.5123));
    }

    #[test]
    fn synthetic_budget_stop_without_structured_output_is_budget_exceeded() {
        // is_error false, no structured_output: only the subtype says why.
        let line = r#"{"type":"result","subtype":"error_max_budget_usd","is_error":false,"total_cost_usd":1.01,"num_turns":9}"#;
        let (kind, _, usage) = failed(line);
        assert_eq!(kind, FailureKind::BudgetExceeded);
        assert_eq!(usage.unwrap().total_cost_usd, Some(1.01));

        // A terminal reason that mentions the budget counts too.
        let line = r#"{"type":"result","subtype":"success","is_error":false,"terminal_reason":"budget_exceeded"}"#;
        assert_eq!(failed(line).0, FailureKind::BudgetExceeded);
    }

    #[test]
    fn synthetic_max_turns_stop_says_the_turn_limit_was_reached() {
        let line = r#"{"type":"result","subtype":"error_max_turns","is_error":true,"result":"","num_turns":30,"total_cost_usd":0.2}"#;
        let (kind, message, _) = failed(line);
        assert_eq!(kind, FailureKind::AgentError);
        assert_eq!(
            message,
            "Claude Code stopped after reaching its turn limit (30 turns) without finishing."
        );
    }

    #[test]
    fn synthetic_other_error_names_its_subtype_and_reason() {
        let line = r#"{"type":"result","subtype":"error_during_execution","is_error":true,"result":"","terminal_reason":"api_error"}"#;
        let (kind, message, _) = failed(line);
        assert_eq!(kind, FailureKind::AgentError);
        assert_eq!(
            message,
            "Claude Code reported an error (subtype error_during_execution, terminal reason api_error)."
        );
    }

    #[test]
    fn unknown_and_noise_lines_are_ignored() {
        for line in [
            "",
            "not json",
            r#"{"type":"rate_limit_event","rate_limit_info":{"status":"allowed"}}"#,
            r#"{"type":"system","subtype":"thinking_tokens","tokens":12}"#,
            r#"{"type":"system","subtype":"hook_started","hook_name":"x"}"#,
            r#"{"type":"something_new"}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"secret thoughts"}]}}"#,
        ] {
            assert_eq!(parse_line(line), None, "{line}");
        }
    }

    #[test]
    fn tool_use_progress_names_the_step_not_the_content() {
        let line = r#"{"type":"assistant","message":{"content":[
            {"type":"tool_use","name":"Read","input":{"file_path":"C:\\skills\\demo\\SKILL.md"}},
            {"type":"tool_use","name":"Grep","input":{"pattern":"TODO","path":"/x"}}]}}"#
            .replace('\n', "");
        assert_eq!(
            parse_line(&line),
            Some(RunEvent::Progress {
                text: "Read SKILL.md; Grep TODO".into()
            })
        );
    }

    fn sample_schema() -> Value {
        // Characters that break naive cmd.exe quoting: quotes, spaces, & | < >
        // ^ % ! and parentheses.
        serde_json::json!({
            "type": "object",
            "required": ["verdict", "grade"],
            "properties": {
                "verdict": {"type": "string", "description": "One line: \"what\" & why (<= 200 chars) | no ^carets^, 100% sure!"},
                "grade": {"type": "string", "enum": ["A", "B", "C", "D", "F"]},
                "score": {"type": "object", "properties": {"value": {"type": "integer"}, "max": {"type": "integer"}}}
            },
            "additionalProperties": false
        })
    }

    fn sample_request(cwd: &Path, extra: Vec<PathBuf>) -> RunRequest {
        RunRequest {
            cwd: cwd.to_path_buf(),
            extra_read_dirs: extra,
            prompt: "Evaluate.".into(),
            output_schema: sample_schema(),
            model: Some("claude-opus-5-5".into()),
            timeout: Duration::from_secs(600),
            budget_usd: Some(2.5),
        }
    }

    #[test]
    fn command_line_is_read_only_and_carries_the_minified_schema() {
        let req = sample_request(
            Path::new("run-dir"),
            vec![PathBuf::from("target dir"), PathBuf::from("evaluator")],
        );
        let args = build_args(&req).unwrap();
        let schema = serde_json::to_string(&sample_schema()).unwrap();
        let expected: Vec<OsString> = [
            "-p",
            "--output-format",
            "stream-json",
            "--verbose",
            "--restricted",
            "--tools",
            "Read,Glob,Grep",
            "--permission-prompts",
            "none",
            "--strict-mcp-config",
            "--no-session-persistence",
            "--add-dir",
            "target dir",
            "--add-dir",
            "evaluator",
            "--json-schema",
            &schema,
            "--model",
            "claude-opus-5-5",
            "--max-budget-usd",
            "2.5",
        ]
        .iter()
        .map(OsString::from)
        .collect();
        assert_eq!(args, expected);
        assert!(!schema.contains('\n'));

        let cmd = ClaudeCodeAdapter
            .build_command(&Launch::Direct(PathBuf::from("claude")), &req)
            .unwrap();
        assert_eq!(cmd.get_program(), "claude");
        assert_eq!(cmd.get_args().collect::<Vec<_>>(), expected);
        assert_eq!(cmd.get_current_dir(), Some(Path::new("run-dir")));

        // Every session marker this process has (inside a Claude Code session
        // there are several) is removed from the agent's environment.
        let removed: Vec<OsString> = cmd
            .get_envs()
            .filter(|(_, v)| v.is_none())
            .map(|(k, _)| k.to_owned())
            .collect();
        for (key, _) in std::env::vars_os().filter(|(k, _)| is_scrubbed(k)) {
            assert!(removed.contains(&key), "{key:?} is not removed");
        }
    }

    #[test]
    fn optional_model_and_budget_are_omitted_when_unset() {
        let mut req = sample_request(Path::new("."), vec![]);
        req.model = None;
        req.budget_usd = None;
        let args = build_args(&req).unwrap();
        assert!(!args
            .iter()
            .any(|a| a == "--model" || a == "--max-budget-usd"));
    }

    #[test]
    fn scrubbing_matches_session_markers_only() {
        for key in [
            "CLAUDECODE",
            "CLAUDE_PID",
            "CLAUDE_EFFORT",
            "CLAUDE_CODE_ENTRYPOINT",
            "claude_code_ssE_port",
            "CLAUDE_AGENT_SDK_VERSION",
        ] {
            assert!(is_scrubbed(OsStr::new(key)), "{key} should be scrubbed");
        }
        for key in [
            "ANTHROPIC_API_KEY",
            "ANTHROPIC_BASE_URL",
            "CLAUDE_CONFIG_DIR",
            "CLAUDE",
            "PATH",
        ] {
            assert!(!is_scrubbed(OsStr::new(key)), "{key} should be kept");
        }
    }

    /// A real child prints its environment: the session markers are gone,
    /// credentials and config survive.
    #[test]
    fn scrubbed_child_environment() {
        let mut cmd = if cfg!(windows) {
            let mut c = crate::core::process::hidden_command("cmd");
            c.args(["/D", "/C", "set"]);
            c
        } else {
            crate::core::process::hidden_command("env")
        };
        cmd.env("CLAUDECODE", "1")
            .env("CLAUDE_CODE_ENTRYPOINT", "cli")
            .env("CLAUDE_AGENT_SDK_VERSION", "0.1")
            .env("CLAUDE_PID", "4242")
            .env("CLAUDE_EFFORT", "high")
            .env("ANTHROPIC_API_KEY", "keep-key")
            .env("ANTHROPIC_BASE_URL", "https://keep.invalid")
            .env("CLAUDE_CONFIG_DIR", "keep-config");
        scrub_env(&mut cmd);
        let out = cmd.output().unwrap();
        let env = String::from_utf8_lossy(&out.stdout);
        let names: Vec<String> = env
            .lines()
            .filter_map(|l| l.split_once('='))
            .map(|(k, _)| k.to_ascii_uppercase())
            .collect();
        for gone in [
            "CLAUDECODE",
            "CLAUDE_CODE_ENTRYPOINT",
            "CLAUDE_AGENT_SDK_VERSION",
            "CLAUDE_PID",
            "CLAUDE_EFFORT",
        ] {
            assert!(!names.iter().any(|n| n == gone), "{gone} reached the child");
        }
        assert!(names.iter().all(|n| !is_scrubbed(OsStr::new(n))));
        for kept in [
            "ANTHROPIC_API_KEY=keep-key",
            "ANTHROPIC_BASE_URL=https://keep.invalid",
            "CLAUDE_CONFIG_DIR=keep-config",
        ] {
            assert!(env.lines().any(|l| l == kept), "{kept} missing");
        }
    }

    #[cfg(windows)]
    /// Prints its arguments as a JSON array, one line.
    const ECHO_ARGS_JS: &str =
        "process.stdout.write(JSON.stringify(process.argv.slice(2)) + \"\\n\");\n";

    #[cfg(windows)]
    fn node_on_path() -> Option<PathBuf> {
        let name = if cfg!(windows) { "node.exe" } else { "node" };
        std::env::split_paths(&std::env::var_os("PATH")?)
            .map(|d| d.join(name))
            .find(|p| p.is_file())
    }

    #[cfg(windows)]
    /// Run the real adapter command for `launch` and return the arguments
    /// the far end received.
    fn echoed_args(launch: &Launch, req: &RunRequest) -> Vec<String> {
        let out = ClaudeCodeAdapter
            .build_command(launch, req)
            .unwrap()
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "echo child failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        serde_json::from_str(stdout.trim()).unwrap_or_else(|e| panic!("{e}: {stdout}"))
    }

    #[cfg(windows)]
    fn expected_args(req: &RunRequest) -> Vec<String> {
        build_args(req)
            .unwrap()
            .into_iter()
            .map(|a| a.into_string().unwrap())
            .collect()
    }

    /// Through an npm shim, the schema reaches the agent byte for byte on both
    /// paths: the parsed shim (node + entry, no cmd.exe) and the opaque shim
    /// (cmd.exe via std's batch escaping).
    #[cfg(windows)]
    #[test]
    fn schema_survives_both_shim_launch_paths_byte_for_byte() {
        if node_on_path().is_none() {
            eprintln!("skipped: node is not on PATH");
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let req = sample_request(tmp.path(), vec![tmp.path().join("target dir")]);
        std::fs::create_dir_all(tmp.path().join("target dir")).unwrap();
        let expected = expected_args(&req);
        let schema = serde_json::to_string(&sample_schema()).unwrap();

        // 1. An npm shim we can read: launched as node + entry directly.
        let entry_dir = tmp.path().join("node_modules").join("fake-cli");
        std::fs::create_dir_all(&entry_dir).unwrap();
        std::fs::write(entry_dir.join("cli.js"), ECHO_ARGS_JS).unwrap();
        let shim = tmp.path().join("claude.cmd");
        std::fs::write(
            &shim,
            "@ECHO off\r\nGOTO start\r\n:find_dp0\r\nSET dp0=%~dp0\r\nEXIT /b\r\n:start\r\nSETLOCAL\r\nCALL :find_dp0\r\n\r\nIF EXIST \"%dp0%\\node.exe\" (\r\n  SET \"_prog=%dp0%\\node.exe\"\r\n) ELSE (\r\n  SET \"_prog=node\"\r\n  SET PATHEXT=%PATHEXT:;.JS;=;%\r\n)\r\n\r\nendLocal & goto #_undefined_# 2>NUL || title %COMSPEC% & \"%_prog%\"  \"%dp0%\\node_modules\\fake-cli\\cli.js\" %*\r\n",
        )
        .unwrap();
        let launch = resolve::launch_for(&shim);
        assert!(matches!(launch, Launch::Node { .. }), "{launch:?}");
        let got = echoed_args(&launch, &req);
        assert_eq!(got, expected);
        let at = got.iter().position(|a| a == "--json-schema").unwrap();
        assert_eq!(got[at + 1].as_bytes(), schema.as_bytes());

        // 2. A shim we cannot read: spawned as a batch file.
        let opaque = tmp.path().join("opaque.cmd");
        std::fs::write(
            &opaque,
            "@echo off\r\nset \"ENTRY=%~dp0node_modules\\fake-cli\\cli.js\"\r\nnode \"%ENTRY%\" %*\r\n",
        )
        .unwrap();
        let launch = resolve::launch_for(&opaque);
        assert_eq!(launch, Launch::BatchShim(opaque.clone()));
        let got = echoed_args(&launch, &req);
        assert_eq!(got, expected);
        let at = got.iter().position(|a| a == "--json-schema").unwrap();
        assert_eq!(got[at + 1].as_bytes(), schema.as_bytes());
    }
}
