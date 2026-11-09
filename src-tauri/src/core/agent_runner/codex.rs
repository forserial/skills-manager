//! The Codex adapter: `codex exec --json`. EXPERIMENTAL.
//!
//! Nothing here has been run against a real Codex install: Codex was not
//! installed on the development machine and running it was out of bounds for
//! sm-9dn.6. The command line and the event shapes come from the openai/codex
//! documentation and source, read 2026-11-09:
//!
//! - <https://learn.chatgpt.com/docs/non-interactive-mode> (formerly
//!   developers.openai.com/codex/noninteractive): `--json`, `--output-schema`,
//!   `--sandbox`, `--skip-git-repo-check`, `-` for the prompt on stdin.
//! - `codex-rs/exec/src/cli.rs` and `codex-rs/utils/cli/src/shared_options.rs`
//!   on `main`: `-C/--cd`, `-m/--model`, `--add-dir` (writable dirs only).
//! - `codex-rs/exec/src/exec_events.rs` on `main`: the JSONL event types.
//! - `codex-rs/protocol/src/protocol.rs` on `main`:
//!   `SandboxPolicy::has_full_disk_read_access` is `true` for every policy,
//!   read-only included, and read-only has no network by default.
//!
//! What this adapter relies on:
//!
//! - Command: `codex exec --json --sandbox read-only --skip-git-repo-check
//!   --output-schema <file> -C <run dir> [-m <model>] -`, prompt on stdin.
//! - `--output-schema` takes a file. The strict schema is written into the
//!   run directory (see [`SCHEMA_FILE_NAME`]).
//! - Read-only allows reading the whole disk, so `extra_read_dirs` need no
//!   flag. (`--add-dir` exists but grants *write* access; it is never used.)
//! - Codex has no spend cap, so `budget_usd` is ignored. Cost is unknown.
//! - Events are `{"type": ...}` lines: `thread.started`, `turn.started`,
//!   `item.started` / `item.updated` / `item.completed` (with an `item` whose
//!   own `type` is `agent_message`, `reasoning`, `command_execution`, ...),
//!   `turn.completed` (with `usage`), `turn.failed` (with `error.message`),
//!   and `error` (with `message`). Unknown ones are ignored.
//! - The answer is the text of the last `agent_message`, parsed as JSON. The
//!   token usage comes after it, on `turn.completed`, so the per-run parser
//!   ([`AgentAdapter::line_parser`]) holds the answer until then.
//! - Codex's structured output is strict (every property required, no extra
//!   properties), so optional fields come back as `null`. Those nulls are
//!   removed before the answer is reported (see [`strip_nulls`]).

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Map, Value};

use super::resolve::Launch;
use super::{AgentAdapter, FailureKind, RunEvent, RunRequest, RunUsage};

/// The file the strict output schema is written to, inside the run
/// directory (`RunRequest::cwd`).
///
/// The run directory is a per-run empty temporary directory that the caller
/// deletes after the run, so the file is cleaned up with it, never lands in a
/// skill, and cannot collide with another run. Codex is sandboxed read-only
/// there, so it can see the file but not change it; seeing it is harmless.
pub const SCHEMA_FILE_NAME: &str = ".codex-output-schema.json";

const AUTH_MESSAGE: &str = "Codex is not signed in, or its credentials were rejected. \
     Run `codex login` in a terminal, then try again.";

/// Longest argument shown in a progress line.
const PROGRESS_ARG_CHARS: usize = 60;

/// Longest agent-reported error message passed on.
const ERROR_MESSAGE_CHARS: usize = 1000;

/// How much of a non-JSON final message goes into the error.
const ANSWER_EXCERPT_CHARS: usize = 200;

pub struct CodexAdapter;

impl AgentAdapter for CodexAdapter {
    fn key(&self) -> &'static str {
        "codex"
    }

    fn display_name(&self) -> &'static str {
        "Codex"
    }

    fn binary_name(&self) -> &'static str {
        "codex"
    }

    /// Never run against a real Codex install yet. Remove once a real
    /// evaluation run through Codex has succeeded (sm-9dn.6).
    fn experimental(&self) -> bool {
        true
    }

    fn build_command(&self, launch: &Launch, request: &RunRequest) -> Result<Command, String> {
        // `-C` is resolved by Codex against its own working directory, which
        // is also the run directory, so a relative path would point inside
        // itself. Pass it absolute.
        let cwd = absolute(&request.cwd);
        let schema_path = write_schema_file(&cwd, &request.output_schema)?;
        if request.budget_usd.is_some() {
            log::info!("Codex has no spend cap; the run's budget is not enforced");
        }
        let mut cmd = launch.command();
        cmd.args(build_args(request, &cwd, &schema_path));
        cmd.current_dir(&cwd);
        Ok(cmd)
    }

    /// The single-line view: an `agent_message` holding a JSON object is a
    /// `Finished` without usage, and `turn.completed` gives nothing because
    /// the answer is on an earlier line. The runner uses
    /// [`line_parser`](AgentAdapter::line_parser), which pairs the two.
    fn parse_line(&self, line: &str) -> Option<RunEvent> {
        match classify(line) {
            Line::Event(event) => Some(event),
            Line::Answer(Ok(output_json)) => Some(RunEvent::Finished {
                output_json,
                usage: RunUsage::default(),
            }),
            Line::Answer(Err(_)) | Line::TurnCompleted(_) | Line::Ignore => None,
        }
    }

    fn line_parser(&self) -> Box<dyn FnMut(&str) -> Option<RunEvent> + '_> {
        let mut run = CodexRun::default();
        Box::new(move |line| run.feed(line))
    }
}

/// The arguments after the program (and after the entry script for a node
/// launch). The prompt is not among them: it goes on stdin (`-`).
pub fn build_args(request: &RunRequest, cwd: &Path, schema_path: &Path) -> Vec<OsString> {
    let mut args: Vec<OsString> = [
        "exec",
        "--json",
        "--sandbox",
        "read-only",
        "--skip-git-repo-check",
    ]
    .iter()
    .map(OsString::from)
    .collect();
    args.push("--output-schema".into());
    args.push(schema_path.as_os_str().to_owned());
    args.push("-C".into());
    args.push(cwd.as_os_str().to_owned());
    if let Some(model) = request
        .model
        .as_deref()
        .map(str::trim)
        .filter(|m| !m.is_empty())
    {
        args.push("-m".into());
        args.push(model.into());
    }
    // Read the whole prompt from stdin.
    args.push("-".into());
    args
}

fn absolute(path: &Path) -> PathBuf {
    if path.is_absolute() {
        return path.to_path_buf();
    }
    std::env::current_dir()
        .map(|dir| dir.join(path))
        .unwrap_or_else(|_| path.to_path_buf())
}

/// Write the strict variant of `schema` into the run directory and return
/// its path.
fn write_schema_file(cwd: &Path, schema: &Value) -> Result<PathBuf, String> {
    let path = cwd.join(SCHEMA_FILE_NAME);
    let text = serde_json::to_string_pretty(&strict_schema(schema))
        .map_err(|e| format!("The output schema cannot be serialized: {e}"))?;
    std::fs::write(&path, text).map_err(|e| {
        format!(
            "Could not write the output schema for Codex to {}: {e}",
            path.display()
        )
    })?;
    Ok(path)
}

// ---------------------------------------------------------------------------
// Strict schema

/// The strict variant of a JSON Schema, as Codex's `--output-schema` (OpenAI
/// structured outputs in strict mode) wants it:
///
/// - every object gets `additionalProperties: false`;
/// - every property is listed in `required`;
/// - a property that was optional is made nullable: `"type": [t, "null"]`
///   for a plain scalar, otherwise `anyOf: [<schema>, {"type": "null"}]`
///   (objects, arrays, enums, consts, `$ref`s).
///
/// Subschemas under `properties`, `items`, `prefixItems`, `anyOf`, `oneOf`,
/// `allOf`, `$defs` and `definitions` are transformed too. `input` is not
/// changed.
pub fn strict_schema(input: &Value) -> Value {
    let mut schema = input.clone();
    make_strict(&mut schema);
    schema
}

const SCALAR_TYPES: [&str; 4] = ["string", "number", "integer", "boolean"];

fn make_strict(schema: &mut Value) {
    let Value::Object(map) = schema else {
        return;
    };

    for key in ["anyOf", "oneOf", "allOf", "prefixItems"] {
        if let Some(Value::Array(subs)) = map.get_mut(key) {
            subs.iter_mut().for_each(make_strict);
        }
    }
    for key in ["$defs", "definitions"] {
        if let Some(Value::Object(defs)) = map.get_mut(key) {
            defs.values_mut().for_each(make_strict);
        }
    }
    match map.get_mut("items") {
        Some(Value::Array(subs)) => subs.iter_mut().for_each(make_strict),
        Some(sub @ Value::Object(_)) => make_strict(sub),
        _ => {}
    }

    if !is_object_schema(map) {
        return;
    }
    let originally_required: Vec<String> = match map.get("required") {
        Some(Value::Array(names)) => names
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect(),
        _ => Vec::new(),
    };
    let mut properties = match map.remove("properties") {
        Some(Value::Object(props)) => props,
        _ => Map::new(),
    };
    let mut required = originally_required.clone();
    for (name, sub) in properties.iter_mut() {
        make_strict(sub);
        if !originally_required.contains(name) {
            make_nullable(sub);
            required.push(name.clone());
        }
    }
    map.insert("properties".into(), Value::Object(properties));
    map.insert(
        "required".into(),
        Value::Array(required.into_iter().map(Value::String).collect()),
    );
    map.insert("additionalProperties".into(), Value::Bool(false));
}

fn is_object_schema(map: &Map<String, Value>) -> bool {
    match map.get("type") {
        Some(Value::String(t)) => t == "object",
        Some(Value::Array(types)) => types.iter().any(|t| t == "object"),
        _ => map.contains_key("properties"),
    }
}

fn allows_null(schema: &Value) -> bool {
    match schema.get("type") {
        Some(Value::String(t)) if t == "null" => return true,
        Some(Value::Array(types)) if types.iter().any(|t| t == "null") => return true,
        _ => {}
    }
    match schema.get("anyOf") {
        Some(Value::Array(subs)) => subs.iter().any(allows_null),
        _ => false,
    }
}

fn make_nullable(schema: &mut Value) {
    if allows_null(schema) {
        return;
    }
    if let Value::Object(map) = schema {
        // A bare scalar type can take "null" in its type list. With an enum
        // or const, null would still be rejected by the value list, so those
        // take the anyOf form below.
        let plain = !map.contains_key("enum") && !map.contains_key("const");
        let scalar = |t: &Value| t.as_str().is_some_and(|t| SCALAR_TYPES.contains(&t));
        let with_null = match map.get("type") {
            Some(t @ Value::String(_)) if plain && scalar(t) => Some(vec![t.clone()]),
            Some(Value::Array(types)) if plain && !types.is_empty() && types.iter().all(scalar) => {
                Some(types.clone())
            }
            _ => None,
        };
        if let Some(mut types) = with_null {
            types.push(Value::String("null".into()));
            map.insert("type".into(), Value::Array(types));
            return;
        }
    }
    let inner = std::mem::take(schema);
    *schema = serde_json::json!({ "anyOf": [inner, { "type": "null" }] });
}

/// Remove every `null`-valued key from every object, at any depth. Codex
/// fills absent optional fields with `null` (the strict schema forces every
/// field to be present), so this gives the app the same shape a Claude run
/// gives: absent means absent. `null` elements of arrays are kept.
///
/// A field that was *required* and nullable in the caller's schema would
/// lose its `null` too; the v1 evaluation contract has no such field.
pub fn strip_nulls(value: &mut Value) {
    match value {
        Value::Object(map) => {
            map.retain(|_, v| !v.is_null());
            map.values_mut().for_each(strip_nulls);
        }
        Value::Array(items) => items.iter_mut().for_each(strip_nulls),
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// Event parsing

/// What one `--json` line means, before pairing the answer with the usage.
#[derive(Debug, PartialEq)]
enum Line {
    Event(RunEvent),
    /// An `agent_message`: the answer if it is a JSON object, otherwise why
    /// not. Only the last one counts.
    Answer(Result<Value, String>),
    TurnCompleted(RunUsage),
    Ignore,
}

/// Per-run state: the latest answer, reported with the usage when the turn
/// completes.
#[derive(Default)]
struct CodexRun {
    answer: Option<Result<Value, String>>,
}

impl CodexRun {
    fn feed(&mut self, line: &str) -> Option<RunEvent> {
        match classify(line) {
            Line::Event(event) => Some(event),
            Line::Answer(answer) => {
                // Report a usable answer at once, in case `turn.completed`
                // never arrives; it is replaced by the one with usage.
                let event = answer.as_ref().ok().map(|output_json| RunEvent::Finished {
                    output_json: output_json.clone(),
                    usage: RunUsage::default(),
                });
                self.answer = Some(answer);
                event
            }
            Line::TurnCompleted(usage) => Some(match self.answer.take() {
                Some(Ok(output_json)) => RunEvent::Finished { output_json, usage },
                Some(Err(message)) => RunEvent::Failed {
                    kind: FailureKind::InvalidOutput,
                    message,
                    usage: Some(usage),
                },
                None => RunEvent::Failed {
                    kind: FailureKind::InvalidOutput,
                    message: "Codex finished without a final message.".to_string(),
                    usage: Some(usage),
                },
            }),
            Line::Ignore => None,
        }
    }
}

fn classify(line: &str) -> Line {
    let Ok(event) = serde_json::from_str::<Value>(line.trim()) else {
        return Line::Ignore;
    };
    let Some(kind) = event.get("type").and_then(Value::as_str) else {
        return Line::Ignore;
    };
    let text_at = |pointer: &str| event.pointer(pointer).and_then(Value::as_str).unwrap_or("");
    match kind {
        "thread.started" => Line::Event(RunEvent::Progress {
            text: "Session started".to_string(),
        }),
        "item.started" => item_started(event.get("item")),
        "item.completed" => item_completed(event.get("item")),
        "turn.completed" => Line::TurnCompleted(RunUsage {
            usage: event.get("usage").cloned(),
            ..RunUsage::default()
        }),
        "turn.failed" => Line::Event(failure_from(text_at("/error/message"))),
        "error" => Line::Event(failure_from(text_at("/message"))),
        // turn.started, item.updated, and anything newer.
        _ => Line::Ignore,
    }
}

/// An item's kind. Before Codex 0.44 the field was `item_type`.
fn item_kind(item: &Value) -> Option<&str> {
    item.get("type")
        .or_else(|| item.get("item_type"))
        .and_then(Value::as_str)
}

fn item_str<'a>(item: &'a Value, key: &str) -> Option<&'a str> {
    item.get(key).and_then(Value::as_str)
}

/// A short line for a step the agent starts. Never the content.
fn item_started(item: Option<&Value>) -> Line {
    let Some(item) = item else {
        return Line::Ignore;
    };
    let text = match item_kind(item) {
        Some("command_execution") => match item_str(item, "command") {
            Some(command) => format!("Run {}", truncate(command, PROGRESS_ARG_CHARS)),
            None => "Run a command".to_string(),
        },
        Some("web_search") => match item_str(item, "query") {
            Some(query) => format!("Web search {}", truncate(query, PROGRESS_ARG_CHARS)),
            None => "Web search".to_string(),
        },
        Some("mcp_tool_call") => {
            let server = item_str(item, "server").unwrap_or("mcp");
            let tool = item_str(item, "tool").unwrap_or("tool");
            format!(
                "Tool {}",
                truncate(&format!("{server}.{tool}"), PROGRESS_ARG_CHARS)
            )
        }
        _ => return Line::Ignore,
    };
    Line::Event(RunEvent::Progress { text })
}

fn item_completed(item: Option<&Value>) -> Line {
    let Some(item) = item else {
        return Line::Ignore;
    };
    match item_kind(item) {
        // `assistant_message` is the pre-0.44 name.
        Some("agent_message" | "assistant_message") => {
            Line::Answer(parse_answer(item_str(item, "text").unwrap_or("")))
        }
        // A non-fatal error item; fatal ones come as `error`/`turn.failed`.
        Some("error") => match item_str(item, "message").map(str::trim) {
            Some(message) if !message.is_empty() => Line::Event(RunEvent::Progress {
                text: format!("Codex warning: {}", truncate(message, PROGRESS_ARG_CHARS)),
            }),
            _ => Line::Ignore,
        },
        // Reasoning (the agent's thoughts), command output, and the rest.
        _ => Line::Ignore,
    }
}

/// The answer in a final message: a JSON object, nulls removed. A fenced
/// ```json block is tolerated.
fn parse_answer(text: &str) -> Result<Value, String> {
    let text = text.trim();
    match serde_json::from_str::<Value>(strip_code_fence(text)) {
        Ok(mut answer @ Value::Object(_)) => {
            strip_nulls(&mut answer);
            Ok(answer)
        }
        _ => Err(format!(
            "Codex's final message is not a JSON object: {}",
            truncate(text, ANSWER_EXCERPT_CHARS)
        )),
    }
}

fn strip_code_fence(text: &str) -> &str {
    let Some(rest) = text.strip_prefix("```") else {
        return text;
    };
    let Some((_, body)) = rest.split_once('\n') else {
        return text;
    };
    body.trim_end().strip_suffix("```").unwrap_or(body).trim()
}

/// Whether an error message looks like rejected or missing credentials.
/// A guess from the shape of HTTP errors; not seen from a real run.
fn looks_like_auth(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    [
        "status 401",
        "401 unauthorized",
        "unauthorized",
        "not logged in",
        "codex login",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

fn failure_from(message: &str) -> RunEvent {
    let message = message.trim();
    if looks_like_auth(message) {
        return RunEvent::Failed {
            kind: FailureKind::Auth,
            message: format!(
                "{AUTH_MESSAGE} (Codex said: {})",
                truncate(message, ERROR_MESSAGE_CHARS)
            ),
            usage: None,
        };
    }
    RunEvent::Failed {
        kind: FailureKind::AgentError,
        message: if message.is_empty() {
            "Codex reported an error.".to_string()
        } else {
            truncate(message, ERROR_MESSAGE_CHARS)
        },
        usage: None,
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
    //! The fixtures `testdata/codex_*.jsonl` and every inline event line here
    //! are SYNTHETIC: hand-written from the Codex docs and `exec_events.rs`,
    //! not captured from a real run. Replace them with captured output once
    //! Codex has been run for real.

    use super::*;
    use crate::core::agent_runner::{run, ClaudeCodeAdapter, RunContext, RunOutput};
    use crate::core::error::{AppError, ErrorKind};
    use crate::core::process::tests::shell;
    use serde_json::json;
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;
    use std::time::Duration;

    const SUCCESS: &str = include_str!("testdata/codex_success.jsonl");
    const FAILURE: &str = include_str!("testdata/codex_failure.jsonl");

    /// A local copy of the v1 evaluation contract's shape. The real constant
    /// lives in `core/evaluation.rs` (sm-9dn.3).
    fn v1_schema() -> Value {
        json!({
            "type": "object",
            "required": ["schema_version", "verdict", "report_markdown"],
            "properties": {
                "schema_version": {"type": "integer", "const": 1},
                "verdict": {"type": "string"},
                "score": {
                    "type": "object",
                    "required": ["value", "max"],
                    "properties": {
                        "value": {"type": "number"},
                        "max": {"type": "number"}
                    }
                },
                "grade": {"type": "string"},
                "dimensions": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "required": ["id", "name", "score", "max"],
                        "properties": {
                            "id": {"type": "string"},
                            "name": {"type": "string"},
                            "score": {"type": "number"},
                            "max": {"type": "number"},
                            "note": {"type": "string"}
                        }
                    }
                },
                "findings": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "required": ["severity", "title"],
                        "properties": {
                            "severity": {"type": "string", "enum": ["critical", "major", "minor", "info"]},
                            "title": {"type": "string"},
                            "detail": {"type": "string"},
                            "location": {"type": "string"},
                            "suggestion": {"type": "string"}
                        }
                    }
                },
                "report_markdown": {"type": "string"}
            },
            "additionalProperties": false
        })
    }

    fn request(cwd: &Path) -> RunRequest {
        RunRequest {
            cwd: cwd.to_path_buf(),
            extra_read_dirs: vec![cwd.join("target skill"), cwd.join("evaluator")],
            prompt: "Evaluate.".into(),
            output_schema: v1_schema(),
            model: Some("gpt-5.5".into()),
            timeout: Duration::from_secs(600),
            budget_usd: Some(2.5),
        }
    }

    static CODEX: CodexAdapter = CodexAdapter;

    fn feed_all(fixture: &str) -> Vec<RunEvent> {
        let mut parse = CODEX.line_parser();
        fixture.lines().filter_map(|l| parse(l)).collect()
    }

    fn sorted_names(value: &Value) -> Vec<String> {
        let mut names: Vec<String> = value
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        names.sort();
        names
    }

    // --- Command construction ---------------------------------------------

    #[test]
    fn command_line_is_read_only_and_points_at_the_schema_file() {
        let tmp = tempfile::tempdir().unwrap();
        let req = request(tmp.path());
        let cmd = CodexAdapter
            .build_command(&Launch::Direct(PathBuf::from("codex")), &req)
            .unwrap();

        let schema_path = tmp.path().join(SCHEMA_FILE_NAME);
        let expected: Vec<OsString> = vec![
            "exec".into(),
            "--json".into(),
            "--sandbox".into(),
            "read-only".into(),
            "--skip-git-repo-check".into(),
            "--output-schema".into(),
            schema_path.clone().into_os_string(),
            "-C".into(),
            tmp.path().as_os_str().to_owned(),
            "-m".into(),
            "gpt-5.5".into(),
            "-".into(),
        ];
        assert_eq!(cmd.get_program(), "codex");
        assert_eq!(cmd.get_args().collect::<Vec<_>>(), expected);
        assert_eq!(cmd.get_current_dir(), Some(tmp.path()));
        // No --add-dir (it grants write access) and no budget flag.
        assert!(!cmd
            .get_args()
            .any(|a| a == "--add-dir" || a.to_string_lossy().contains("budget")));

        // The file holds the strict variant of the request's schema.
        let written: Value =
            serde_json::from_str(&std::fs::read_to_string(&schema_path).unwrap()).unwrap();
        assert_eq!(written, strict_schema(&v1_schema()));
    }

    #[test]
    fn a_blank_or_missing_model_is_omitted() {
        let tmp = tempfile::tempdir().unwrap();
        let schema = tmp.path().join(SCHEMA_FILE_NAME);
        for model in [None, Some("  ".to_string())] {
            let mut req = request(tmp.path());
            req.model = model;
            let args = build_args(&req, tmp.path(), &schema);
            assert!(!args.iter().any(|a| a == "-m"), "{args:?}");
            assert_eq!(args.last().unwrap(), "-");
        }
    }

    #[test]
    fn a_relative_run_dir_is_passed_absolute() {
        let abs = absolute(Path::new("run-dir"));
        assert!(abs.is_absolute());
        assert_eq!(abs, std::env::current_dir().unwrap().join("run-dir"));
    }

    #[test]
    fn an_unwritable_run_dir_is_an_error_naming_the_file() {
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("gone");
        let err = CodexAdapter
            .build_command(&Launch::Direct(PathBuf::from("codex")), &request(&missing))
            .unwrap_err();
        assert!(err.contains(SCHEMA_FILE_NAME), "{err}");
    }

    #[test]
    fn codex_is_experimental_and_claude_is_not() {
        assert!(CodexAdapter.experimental());
        assert!(!ClaudeCodeAdapter.experimental());
        assert_eq!(CodexAdapter.key(), "codex");
        assert_eq!(CodexAdapter.binary_name(), "codex");
    }

    // --- Strict schema ----------------------------------------------------

    /// Every object in `schema` is closed and requires all its properties.
    fn assert_strict_everywhere(schema: &Value, at: &str) {
        let Value::Object(map) = schema else { return };
        if is_object_schema(map) {
            assert_eq!(map["additionalProperties"], false, "{at}");
            let props: Vec<String> = {
                let mut p: Vec<String> = map["properties"]
                    .as_object()
                    .unwrap()
                    .keys()
                    .cloned()
                    .collect();
                p.sort();
                p
            };
            assert_eq!(sorted_names(&map["required"]), props, "{at}");
        }
        for (key, sub) in map {
            match sub {
                Value::Object(_) => assert_strict_everywhere(sub, &format!("{at}/{key}")),
                Value::Array(items) => items
                    .iter()
                    .enumerate()
                    .for_each(|(i, s)| assert_strict_everywhere(s, &format!("{at}/{key}/{i}"))),
                _ => {}
            }
        }
    }

    #[test]
    fn strict_v1_schema_closes_every_object_and_nulls_the_optional_fields() {
        let strict = strict_schema(&v1_schema());
        assert_strict_everywhere(&strict, "#");

        let p = &strict["properties"];
        // Required fields are untouched.
        assert_eq!(p["schema_version"], json!({"type": "integer", "const": 1}));
        assert_eq!(p["verdict"], json!({"type": "string"}));
        // Optional scalars take null in their type list.
        assert_eq!(p["grade"], json!({"type": ["string", "null"]}));
        // Optional objects and arrays take the anyOf form, and are strict
        // inside.
        assert_eq!(p["score"]["anyOf"][1], json!({"type": "null"}));
        assert_eq!(p["score"]["anyOf"][0]["additionalProperties"], false);
        assert_eq!(p["dimensions"]["anyOf"][1], json!({"type": "null"}));
        let dimension = &p["dimensions"]["anyOf"][0]["items"]["properties"];
        assert_eq!(dimension["note"], json!({"type": ["string", "null"]}));
        assert_eq!(dimension["id"], json!({"type": "string"}));
        let finding = &p["findings"]["anyOf"][0]["items"]["properties"];
        assert_eq!(finding["location"], json!({"type": ["string", "null"]}));
        assert_eq!(
            finding["severity"],
            json!({"type": "string", "enum": ["critical", "major", "minor", "info"]})
        );
        // The original required fields come first, in their order.
        assert_eq!(
            strict["required"].as_array().unwrap()[..3],
            [
                json!("schema_version"),
                json!("verdict"),
                json!("report_markdown")
            ]
        );
        // The input is not modified.
        assert_eq!(
            v1_schema()["properties"]["grade"],
            json!({"type": "string"})
        );
    }

    #[test]
    fn strict_schema_handles_enums_type_lists_refs_and_already_nullable_fields() {
        let schema = json!({
            "type": "object",
            "properties": {
                "level": {"type": "string", "enum": ["a", "b"]},
                "count": {"type": ["integer", "number"]},
                "maybe": {"type": ["string", "null"]},
                "either": {"anyOf": [{"type": "string"}, {"type": "null"}]},
                "linked": {"$ref": "#/$defs/thing"},
                "open": {"type": "object", "additionalProperties": true}
            },
            "$defs": {
                "thing": {"type": "object", "properties": {"x": {"type": "string"}}}
            }
        });
        let strict = strict_schema(&schema);
        assert_strict_everywhere(&strict, "#");
        let p = &strict["properties"];
        assert_eq!(
            p["level"],
            json!({"anyOf": [{"type": "string", "enum": ["a", "b"]}, {"type": "null"}]})
        );
        assert_eq!(p["count"], json!({"type": ["integer", "number", "null"]}));
        assert_eq!(p["maybe"], json!({"type": ["string", "null"]}));
        assert_eq!(p["either"], schema["properties"]["either"]);
        assert_eq!(
            p["linked"],
            json!({"anyOf": [{"$ref": "#/$defs/thing"}, {"type": "null"}]})
        );
        assert_eq!(
            p["open"]["anyOf"][0],
            json!({"type": "object", "properties": {}, "required": [], "additionalProperties": false})
        );
        assert_eq!(
            strict["$defs"]["thing"]["properties"]["x"],
            json!({"type": ["string", "null"]})
        );
    }

    #[test]
    fn nulls_are_stripped_at_every_depth_but_not_from_arrays() {
        let mut value = json!({
            "verdict": "ok",
            "grade": null,
            "score": {"value": 3, "max": null},
            "findings": [{"title": "t", "location": null}, null],
            "report_markdown": ""
        });
        strip_nulls(&mut value);
        assert_eq!(
            value,
            json!({
                "verdict": "ok",
                "score": {"value": 3},
                "findings": [{"title": "t"}, null],
                "report_markdown": ""
            })
        );
    }

    // --- Line parsing -----------------------------------------------------

    #[test]
    fn success_fixture_parses_to_progress_then_finished_with_usage() {
        let events = feed_all(SUCCESS);
        assert_eq!(
            events[..2],
            [
                RunEvent::Progress {
                    text: "Session started".into()
                },
                RunEvent::Progress {
                    text: "Run bash -lc 'cat /skills/skill-judge/SKILL.md'".into()
                },
            ]
        );
        // The prose message yields nothing; the JSON one is reported at once
        // without usage, then again with the usage from turn.completed.
        assert_eq!(events.len(), 4, "{events:?}");
        let RunEvent::Finished { usage, .. } = &events[2] else {
            panic!("expected Finished, got {:?}", events[2]);
        };
        assert_eq!(*usage, RunUsage::default());
        let RunEvent::Finished { output_json, usage } = &events[3] else {
            panic!("expected Finished, got {:?}", events[3]);
        };
        assert_eq!(
            *output_json,
            json!({
                "schema_version": 1,
                "verdict": "A correct but thin API cheat-sheet.",
                "score": {"value": 57, "max": 120},
                "grade": "F",
                "findings": [{
                    "severity": "critical",
                    "title": "Description has no WHEN clause",
                    "detail": "States WHAT only.",
                    "location": "SKILL.md:3"
                }],
                "report_markdown": "# Skill Evaluation Report: gif-search\n"
            })
        );
        let tokens = usage.usage.as_ref().unwrap();
        assert_eq!(tokens["input_tokens"], 24763);
        assert_eq!(tokens["output_tokens"], 1220);
        assert_eq!(usage.total_cost_usd, None, "Codex reports no cost");
    }

    #[test]
    fn failure_fixture_says_to_log_in() {
        let events = feed_all(FAILURE);
        assert_eq!(events.len(), 3, "{events:?}");
        for event in &events[1..] {
            let RunEvent::Failed { kind, message, .. } = event else {
                panic!("expected Failed, got {event:?}");
            };
            assert_eq!(*kind, FailureKind::Auth);
            assert!(message.contains("codex login"), "{message}");
            assert!(message.contains("401 Unauthorized"), "{message}");
        }
    }

    #[test]
    fn other_turn_failures_pass_the_message_through() {
        let line = r#"{"type":"turn.failed","error":{"message":"You've hit your usage limit. Try again later."}}"#;
        assert_eq!(
            CodexAdapter.parse_line(line),
            Some(RunEvent::Failed {
                kind: FailureKind::AgentError,
                message: "You've hit your usage limit. Try again later.".into(),
                usage: None,
            })
        );
        let line = r#"{"type":"error","message":""}"#;
        assert_eq!(
            CodexAdapter.parse_line(line),
            Some(RunEvent::Failed {
                kind: FailureKind::AgentError,
                message: "Codex reported an error.".into(),
                usage: None,
            })
        );
    }

    #[test]
    fn a_final_message_that_is_not_json_is_invalid_output_with_usage() {
        let lines = [
            r#"{"type":"item.completed","item":{"id":"item_0","type":"agent_message","text":"{\"verdict\":\"early\"}"}}"#,
            r#"{"type":"item.completed","item":{"id":"item_1","type":"agent_message","text":"Sorry, I could not finish."}}"#,
            r#"{"type":"turn.completed","usage":{"input_tokens":10,"cached_input_tokens":0,"output_tokens":5,"reasoning_output_tokens":0}}"#,
        ];
        let events = feed_all(&lines.join("\n"));
        let Some(RunEvent::Failed {
            kind,
            message,
            usage,
        }) = events.last()
        else {
            panic!("expected Failed, got {events:?}");
        };
        assert_eq!(*kind, FailureKind::InvalidOutput);
        assert!(message.contains("Sorry, I could not finish."), "{message}");
        assert_eq!(
            usage.as_ref().unwrap().usage.as_ref().unwrap()["input_tokens"],
            10
        );
    }

    #[test]
    fn a_turn_without_a_final_message_is_invalid_output() {
        let events = feed_all(r#"{"type":"turn.completed","usage":{"input_tokens":1}}"#);
        assert!(matches!(
            events.as_slice(),
            [RunEvent::Failed {
                kind: FailureKind::InvalidOutput,
                usage: Some(_),
                ..
            }]
        ));
    }

    #[test]
    fn the_stateless_view_reports_the_answer_and_skips_the_usage_line() {
        let answer = r#"{"type":"item.completed","item":{"id":"i","type":"agent_message","text":"{\"verdict\":\"v\",\"grade\":null}"}}"#;
        assert_eq!(
            CodexAdapter.parse_line(answer),
            Some(RunEvent::Finished {
                output_json: json!({"verdict": "v"}),
                usage: RunUsage::default(),
            })
        );
        assert_eq!(
            CodexAdapter.parse_line(r#"{"type":"turn.completed","usage":{"input_tokens":1}}"#),
            None
        );
    }

    #[test]
    fn a_fenced_answer_and_the_legacy_item_shape_are_accepted() {
        let line = r#"{"type":"item.completed","item":{"id":"i","item_type":"assistant_message","text":"```json\n{\"verdict\":\"v\"}\n```"}}"#;
        assert_eq!(
            CodexAdapter.parse_line(line),
            Some(RunEvent::Finished {
                output_json: json!({"verdict": "v"}),
                usage: RunUsage::default(),
            })
        );
    }

    #[test]
    fn unknown_and_noise_lines_are_ignored() {
        for line in [
            "",
            "not json",
            r#"{"type":"turn.started"}"#,
            r#"{"type":"something.new","x":1}"#,
            r#"{"type":"item.updated","item":{"id":"i","type":"todo_list","items":[]}}"#,
            r#"{"type":"item.completed","item":{"id":"i","type":"reasoning","text":"secret thoughts"}}"#,
            r#"{"type":"item.completed","item":{"id":"i","type":"command_execution","command":"ls","aggregated_output":"secret","exit_code":0,"status":"completed"}}"#,
        ] {
            assert_eq!(CodexAdapter.parse_line(line), None, "{line}");
        }
    }

    #[test]
    fn started_steps_and_warnings_become_short_progress_lines() {
        let cases = [
            (
                r#"{"type":"item.started","item":{"id":"i","type":"web_search","query":"tenor api"}}"#,
                "Web search tenor api",
            ),
            (
                r#"{"type":"item.started","item":{"id":"i","type":"mcp_tool_call","server":"docs","tool":"search","arguments":{},"status":"in_progress"}}"#,
                "Tool docs.search",
            ),
            (
                r#"{"type":"item.completed","item":{"id":"i","type":"error","message":"Reconnecting 1/5"}}"#,
                "Codex warning: Reconnecting 1/5",
            ),
        ];
        for (line, text) in cases {
            assert_eq!(
                CodexAdapter.parse_line(line),
                Some(RunEvent::Progress { text: text.into() }),
                "{line}"
            );
        }
    }

    // --- Through the runner -----------------------------------------------

    /// Prints a fixture instead of running Codex, and parses it with the
    /// real Codex parser.
    struct ScriptedCodex {
        script: String,
    }

    impl AgentAdapter for ScriptedCodex {
        fn key(&self) -> &'static str {
            "test_codex"
        }
        fn display_name(&self) -> &'static str {
            "Codex"
        }
        fn binary_name(&self) -> &'static str {
            "test-codex"
        }
        fn build_command(&self, _launch: &Launch, request: &RunRequest) -> Result<Command, String> {
            let mut cmd = shell(&self.script);
            cmd.current_dir(&request.cwd);
            Ok(cmd)
        }
        fn parse_line(&self, line: &str) -> Option<RunEvent> {
            CODEX.parse_line(line)
        }
        fn line_parser(&self) -> Box<dyn FnMut(&str) -> Option<RunEvent> + '_> {
            CODEX.line_parser()
        }
    }

    fn run_fixture(fixture: &str, code: i32) -> (Result<RunOutput, AppError>, Vec<RunEvent>) {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("out.jsonl");
        std::fs::write(&file, fixture).unwrap();
        let (script, shell_path) = if cfg!(windows) {
            (
                format!("type \"{}\"& exit /b {code}", file.display()),
                std::env::var("ComSpec").unwrap_or_else(|_| r"C:\Windows\System32\cmd.exe".into()),
            )
        } else {
            (
                format!("cat '{}'; exit {code}", file.display()),
                "/bin/sh".into(),
            )
        };
        let mut req = request(tmp.path());
        req.extra_read_dirs.clear();
        let mut events = Vec::new();
        let result = run(
            &ScriptedCodex { script },
            &req,
            &RunContext {
                binary_override: Some(shell_path),
                proxy_url: None,
            },
            &Arc::new(AtomicBool::new(false)),
            &mut |e| events.push(e),
        );
        (result, events)
    }

    /// The runner pairs the answer with the usage that follows it, through
    /// the per-run parser.
    #[test]
    fn a_run_reports_the_answer_with_the_turn_usage() {
        let (result, events) = run_fixture(SUCCESS, 0);
        let output = result.unwrap();
        assert_eq!(output.output_json["grade"], "F");
        assert!(output.output_json.get("dimensions").is_none());
        assert_eq!(output.usage.usage.as_ref().unwrap()["input_tokens"], 24763);
        assert!(matches!(events.last(), Some(RunEvent::Finished { .. })));
    }

    #[test]
    fn a_run_with_rejected_credentials_says_to_log_in() {
        let (result, events) = run_fixture(FAILURE, 1);
        let err = result.unwrap_err();
        assert_eq!(err.kind, ErrorKind::Agent);
        assert!(err.message.contains("codex login"), "{}", err.message);
        assert!(matches!(
            events.last(),
            Some(RunEvent::Failed {
                kind: FailureKind::Auth,
                ..
            })
        ));
    }
}
