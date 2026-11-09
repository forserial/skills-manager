//! Finding an agent CLI binary, and deciding how to launch it.
//!
//! Lookup order: the `agent_binary_<key>` setting, then PATH (on Windows
//! `.exe`, then `.cmd`, then `.bat`), then known install locations
//! (`~/.local/bin`, the npm global prefix).
//!
//! A `.cmd`/`.bat` result is usually an npm shim. Running it means going
//! through `cmd.exe`, whose quoting rules a JSON argument does not survive
//! well, so the shim is read instead and the program it wraps is launched
//! directly: `node <entry.js> <args>`, or the native `.exe` the shim points
//! at. Only a shim we cannot read is spawned as-is, relying on std's
//! batch-file argument escaping (Rust 1.77+), which refuses with
//! `InvalidInput` rather than mis-quoting.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use regex::Regex;

use crate::core::process::hidden_command;

/// The settings key that overrides where an adapter's binary is found.
pub fn setting_key(adapter_key: &str) -> String {
    format!("agent_binary_{adapter_key}")
}

/// How to start a resolved binary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Launch {
    /// Run the file itself.
    Direct(PathBuf),
    /// An npm shim's JavaScript entry point, run with `node` directly.
    Node { node: PathBuf, entry: PathBuf },
    /// A `.cmd`/`.bat` we could not read, run through std's batch escaping.
    BatchShim(PathBuf),
}

impl Launch {
    /// A hidden command for this launch, with no agent arguments yet.
    pub fn command(&self) -> Command {
        match self {
            Launch::Direct(path) | Launch::BatchShim(path) => hidden_command(path),
            Launch::Node { node, entry } => {
                let mut cmd = hidden_command(node);
                cmd.arg(entry);
                cmd
            }
        }
    }

    /// What is actually executed, for messages and logs.
    pub fn describe(&self) -> String {
        match self {
            Launch::Direct(path) | Launch::BatchShim(path) => path.display().to_string(),
            Launch::Node { node, entry } => format!("{} {}", node.display(), entry.display()),
        }
    }
}

/// Find `binary_name` for the adapter `adapter_key`. `override_path` is the
/// value of the `agent_binary_<key>` setting, if set.
///
/// The error names everything that was looked for.
pub fn resolve_binary(
    adapter_key: &str,
    binary_name: &str,
    override_path: Option<&str>,
) -> Result<PathBuf, String> {
    resolve_in(
        &setting_key(adapter_key),
        binary_name,
        override_path,
        std::env::var_os("PATH").as_deref(),
        &known_dirs(),
    )
}

fn resolve_in(
    setting: &str,
    binary_name: &str,
    override_path: Option<&str>,
    path_var: Option<&OsStr>,
    known: &[PathBuf],
) -> Result<PathBuf, String> {
    if let Some(value) = override_path.map(str::trim).filter(|v| !v.is_empty()) {
        let path = PathBuf::from(value);
        // An explicit choice that is wrong is reported, not silently replaced
        // by some other binary found on PATH.
        return if path.is_file() {
            Ok(path)
        } else {
            Err(format!(
                "The setting {setting} points to {}, which is not a file. \
                 Correct it or clear it to search PATH.",
                path.display()
            ))
        };
    }

    let names = file_names(binary_name);
    let path_dirs: Vec<PathBuf> = path_var
        .map(|p| std::env::split_paths(p).collect())
        .unwrap_or_default();
    if let Some(found) = search(&path_dirs, &names) {
        return Ok(found);
    }
    if let Some(found) = search(known, &names) {
        return Ok(found);
    }

    let names_list = names
        .iter()
        .map(|n| n.to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join(", ");
    let known_list = if known.is_empty() {
        "(none)".to_string()
    } else {
        known
            .iter()
            .map(|d| d.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    };
    Err(format!(
        "Could not find the `{binary_name}` CLI. Looked for: setting {setting} (not set); \
         {names_list} on PATH; then in {known_list}. Install it, or set {setting} to its full path."
    ))
}

/// The file names a binary can have. On Windows only real launchable
/// extensions count: npm also drops an extensionless `sh` script next to the
/// `.cmd`, which Windows cannot run.
fn file_names(binary_name: &str) -> Vec<OsString> {
    if cfg!(windows) {
        ["exe", "cmd", "bat"]
            .iter()
            .map(|ext| OsString::from(format!("{binary_name}.{ext}")))
            .collect()
    } else {
        vec![OsString::from(binary_name)]
    }
}

/// Name-major search: every directory for the first name, then every
/// directory for the next, so a native `.exe` anywhere wins over a shim.
fn search(dirs: &[PathBuf], names: &[OsString]) -> Option<PathBuf> {
    names.iter().find_map(|name| {
        dirs.iter()
            .map(|dir| dir.join(name))
            .find(|candidate| is_executable_file(candidate))
    })
}

fn is_executable_file(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// Install locations outside PATH: a GUI app often does not inherit the
/// login shell's PATH (macOS in particular), so these are checked too.
fn known_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(home) = dirs::home_dir() {
        // Claude Code's native installer.
        dirs.push(home.join(".local").join("bin"));
        #[cfg(not(windows))]
        dirs.push(home.join(".claude").join("local"));
    }
    for var in ["NPM_CONFIG_PREFIX", "npm_config_prefix"] {
        if let Some(prefix) = std::env::var_os(var).filter(|p| !p.is_empty()) {
            let prefix = PathBuf::from(prefix);
            // npm puts global bins in the prefix itself on Windows, in
            // <prefix>/bin elsewhere.
            dirs.push(if cfg!(windows) {
                prefix
            } else {
                prefix.join("bin")
            });
        }
    }
    #[cfg(windows)]
    if let Some(appdata) = std::env::var_os("APPDATA") {
        dirs.push(PathBuf::from(appdata).join("npm"));
    }
    #[cfg(not(windows))]
    {
        if let Some(home) = dirs::home_dir() {
            dirs.push(home.join(".npm-global").join("bin"));
        }
        dirs.push(PathBuf::from("/usr/local/bin"));
        dirs.push(PathBuf::from("/opt/homebrew/bin"));
    }
    let mut seen = std::collections::HashSet::new();
    dirs.retain(|d| seen.insert(d.clone()));
    dirs
}

/// Decide how to start `binary`.
pub fn launch_for(binary: &Path) -> Launch {
    let is_batch = binary
        .extension()
        .and_then(OsStr::to_str)
        .is_some_and(|ext| ext.eq_ignore_ascii_case("cmd") || ext.eq_ignore_ascii_case("bat"));
    if !is_batch {
        return Launch::Direct(binary.to_path_buf());
    }
    let fallback = Launch::BatchShim(binary.to_path_buf());
    let (Some(dir), Ok(script)) = (binary.parent(), std::fs::read(binary)) else {
        return fallback;
    };
    if script.len() > 64 * 1024 {
        return fallback;
    }
    let script = String::from_utf8_lossy(&script);
    match parse_npm_shim(&script) {
        Some(ShimTarget::Exe(rel)) => {
            let exe = join_relative(dir, &rel);
            if exe.is_file() {
                return Launch::Direct(exe);
            }
        }
        Some(ShimTarget::Script(rel)) => {
            let entry = join_relative(dir, &rel);
            let node = Some(dir.join("node.exe"))
                .filter(|p| p.is_file())
                .or_else(|| {
                    let path_dirs: Vec<PathBuf> = std::env::var_os("PATH")
                        .map(|p| std::env::split_paths(&p).collect())
                        .unwrap_or_default();
                    search(&path_dirs, &[OsString::from("node.exe")])
                });
            if let (true, Some(node)) = (entry.is_file(), node) {
                return Launch::Node { node, entry };
            }
        }
        None => {}
    }
    fallback
}

/// Join a shim's `\`-separated relative path onto `dir` component by
/// component, so it means the same thing on every platform.
fn join_relative(dir: &Path, rel: &str) -> PathBuf {
    rel.split(['\\', '/'])
        .filter(|part| !part.is_empty())
        .fold(dir.to_path_buf(), |path, part| path.join(part))
}

#[derive(Debug, PartialEq, Eq)]
enum ShimTarget {
    /// A JavaScript entry point, relative to the shim's directory.
    Script(String),
    /// A native executable, relative to the shim's directory.
    Exe(String),
}

/// Read what an npm `cmd-shim` runs. Both generations are understood:
///
/// ```text
/// "%~dp0\node.exe"  "%~dp0\node_modules\pkg\cli.js" %*
/// ... "%_prog%"  "%dp0%\node_modules\pkg\cli.js" %*
/// "%~dp0node_modules\pkg\bin\tool.exe" %*
/// ```
///
/// The last quoted `%dp0%`-relative path that is a script or an executable
/// (other than `node.exe` itself) is the target.
fn parse_npm_shim(script: &str) -> Option<ShimTarget> {
    static QUOTED_DP0: OnceLock<Regex> = OnceLock::new();
    let re = QUOTED_DP0
        .get_or_init(|| Regex::new(r#"(?i)"%(?:~dp0|dp0%)\\?([^"%]+)""#).expect("valid regex"));
    re.captures_iter(script)
        .filter_map(|c| {
            let rel = c.get(1)?.as_str().trim().to_string();
            let lower = rel.to_ascii_lowercase();
            let file = lower.rsplit(['\\', '/']).next().unwrap_or(&lower);
            if file == "node.exe" || file == "node" {
                None
            } else if [".js", ".cjs", ".mjs"]
                .iter()
                .any(|ext| lower.ends_with(ext))
            {
                Some(ShimTarget::Script(rel))
            } else if lower.ends_with(".exe") {
                Some(ShimTarget::Exe(rel))
            } else {
                None
            }
        })
        .last()
}

#[cfg(test)]
mod tests {
    use super::*;

    const MODERN_SHIM: &str = r#"@ECHO off
GOTO start
:find_dp0
SET dp0=%~dp0
EXIT /b
:start
SETLOCAL
CALL :find_dp0

IF EXIST "%dp0%\node.exe" (
  SET "_prog=%dp0%\node.exe"
) ELSE (
  SET "_prog=node"
  SET PATHEXT=%PATHEXT:;.JS;=;%
)

endLocal & goto #_undefined_# 2>NUL || title %COMSPEC% & "%_prog%"  "%dp0%\node_modules\fake-cli\cli.js" %*
"#;

    const LEGACY_SHIM: &str = "@IF EXIST \"%~dp0\\node.exe\" (\r\n  \"%~dp0\\node.exe\"  \"%~dp0\\node_modules\\pkg\\cli.mjs\" %*\r\n) ELSE (\r\n  node  \"%~dp0\\node_modules\\pkg\\cli.mjs\" %*\r\n)\r\n";

    const NATIVE_SHIM: &str =
        "@ECHO off\r\n\"%~dp0node_modules\\agent\\bin\\agent-win32-x64.exe\" %*\r\n";

    #[test]
    fn parses_modern_legacy_and_native_npm_shims() {
        assert_eq!(
            parse_npm_shim(MODERN_SHIM),
            Some(ShimTarget::Script(r"node_modules\fake-cli\cli.js".into()))
        );
        assert_eq!(
            parse_npm_shim(LEGACY_SHIM),
            Some(ShimTarget::Script(r"node_modules\pkg\cli.mjs".into()))
        );
        assert_eq!(
            parse_npm_shim(NATIVE_SHIM),
            Some(ShimTarget::Exe(
                r"node_modules\agent\bin\agent-win32-x64.exe".into()
            ))
        );
        assert_eq!(
            parse_npm_shim("@echo off\r\nset \"E=%~dp0x.js\"\r\nnode \"%E%\" %*\r\n"),
            None
        );
    }

    fn touch_executable(path: &Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    fn exe_name(base: &str) -> String {
        if cfg!(windows) {
            format!("{base}.exe")
        } else {
            base.to_string()
        }
    }

    #[test]
    fn setting_wins_over_path_and_known_locations() {
        let tmp = tempfile::tempdir().unwrap();
        let on_path = tmp.path().join("path").join(exe_name("agent"));
        let chosen = tmp.path().join("chosen").join(exe_name("my-agent"));
        touch_executable(&on_path);
        touch_executable(&chosen);
        let path_var = std::env::join_paths([on_path.parent().unwrap()]).unwrap();

        let found = resolve_in(
            "agent_binary_test",
            "agent",
            Some(chosen.to_str().unwrap()),
            Some(&path_var),
            &[],
        )
        .unwrap();
        assert_eq!(found, chosen);

        // Blank setting means "not set".
        let found = resolve_in(
            "agent_binary_test",
            "agent",
            Some("  "),
            Some(&path_var),
            &[],
        )
        .unwrap();
        assert_eq!(found, on_path);
    }

    #[test]
    fn a_setting_that_points_nowhere_is_an_error_naming_it() {
        let tmp = tempfile::tempdir().unwrap();
        let on_path = tmp.path().join(exe_name("agent"));
        touch_executable(&on_path);
        let path_var = std::env::join_paths([tmp.path()]).unwrap();
        let missing = tmp.path().join("missing").join(exe_name("agent"));

        let err = resolve_in(
            "agent_binary_test",
            "agent",
            Some(missing.to_str().unwrap()),
            Some(&path_var),
            &[],
        )
        .unwrap_err();
        assert!(err.contains("agent_binary_test"), "{err}");
        assert!(err.contains(&missing.display().to_string()), "{err}");
    }

    #[test]
    fn path_comes_before_known_locations() {
        let tmp = tempfile::tempdir().unwrap();
        let on_path = tmp.path().join("path").join(exe_name("agent"));
        let known = tmp.path().join("known").join(exe_name("agent"));
        touch_executable(&on_path);
        touch_executable(&known);
        let path_var = std::env::join_paths([on_path.parent().unwrap()]).unwrap();
        let known_dirs = [known.parent().unwrap().to_path_buf()];

        let found = resolve_in("s", "agent", None, Some(&path_var), &known_dirs).unwrap();
        assert_eq!(found, on_path);

        let found = resolve_in("s", "agent", None, None, &known_dirs).unwrap();
        assert_eq!(found, known);
    }

    #[cfg(windows)]
    #[test]
    fn windows_prefers_exe_then_cmd_then_bat_and_ignores_extensionless() {
        let tmp = tempfile::tempdir().unwrap();
        let first = tmp.path().join("first");
        let second = tmp.path().join("second");
        // npm's sh shim has no extension and must never be picked on Windows.
        touch_executable(&first.join("agent"));
        touch_executable(&first.join("agent.bat"));
        touch_executable(&first.join("agent.cmd"));
        touch_executable(&second.join("agent.exe"));
        let path_var = std::env::join_paths([&first, &second]).unwrap();

        let found = resolve_in("s", "agent", None, Some(&path_var), &[]).unwrap();
        assert_eq!(found, second.join("agent.exe"));

        std::fs::remove_file(second.join("agent.exe")).unwrap();
        let found = resolve_in("s", "agent", None, Some(&path_var), &[]).unwrap();
        assert_eq!(found, first.join("agent.cmd"));

        std::fs::remove_file(first.join("agent.cmd")).unwrap();
        let found = resolve_in("s", "agent", None, Some(&path_var), &[]).unwrap();
        assert_eq!(found, first.join("agent.bat"));
    }

    #[test]
    fn a_missing_binary_error_names_what_was_looked_for() {
        let tmp = tempfile::tempdir().unwrap();
        let empty_path = std::env::join_paths([tmp.path()]).unwrap();
        let known = [tmp.path().join("known-location")];

        let err = resolve_in(
            "agent_binary_test",
            "agent",
            None,
            Some(&empty_path),
            &known,
        )
        .unwrap_err();
        assert!(err.contains("agent_binary_test"), "{err}");
        assert!(err.contains(&exe_name("agent")), "{err}");
        assert!(err.contains("known-location"), "{err}");
    }

    #[test]
    fn non_batch_binaries_launch_directly() {
        let path = PathBuf::from("/opt/agent/bin/agent");
        assert_eq!(launch_for(&path), Launch::Direct(path));
    }

    #[test]
    fn a_shim_wrapping_a_native_exe_launches_the_exe() {
        let tmp = tempfile::tempdir().unwrap();
        let shim = tmp.path().join("agent.cmd");
        std::fs::write(&shim, NATIVE_SHIM).unwrap();
        let exe = tmp
            .path()
            .join("node_modules")
            .join("agent")
            .join("bin")
            .join("agent-win32-x64.exe");
        touch_executable(&exe);
        assert_eq!(launch_for(&shim), Launch::Direct(exe));
    }

    #[test]
    fn a_node_shim_launches_node_with_the_entry_point() {
        let tmp = tempfile::tempdir().unwrap();
        let shim = tmp.path().join("agent.cmd");
        std::fs::write(&shim, MODERN_SHIM).unwrap();
        let entry = tmp
            .path()
            .join("node_modules")
            .join("fake-cli")
            .join("cli.js");
        touch_executable(&entry);
        // node.exe next to the shim wins over node on PATH.
        let node = tmp.path().join("node.exe");
        touch_executable(&node);
        assert_eq!(launch_for(&shim), Launch::Node { node, entry });
    }

    #[test]
    fn an_unreadable_or_dangling_shim_falls_back_to_the_shim_itself() {
        let tmp = tempfile::tempdir().unwrap();
        let shim = tmp.path().join("agent.cmd");
        // Points at an entry that is not there.
        std::fs::write(&shim, MODERN_SHIM).unwrap();
        assert_eq!(launch_for(&shim), Launch::BatchShim(shim.clone()));

        let opaque = tmp.path().join("opaque.bat");
        std::fs::write(&opaque, "@echo off\r\ncall something-else %*\r\n").unwrap();
        assert_eq!(launch_for(&opaque), Launch::BatchShim(opaque.clone()));
    }
}
