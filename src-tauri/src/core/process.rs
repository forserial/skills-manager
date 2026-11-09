//! A generic child-process helper: a hidden command, plus a watched run with
//! streamed stdout, a deadline, cancellation and a process-tree kill.
//!
//! Modelled on `git_fetcher::run_git_watched`, which stays git-only and
//! untouched. The differences that matter here:
//!
//! - stdout is streamed line by line to the caller while the child runs,
//!   because agent CLIs report progress there;
//! - cancel and timeout kill the whole process tree, not just the direct
//!   child. An agent CLI launched through `node` or a batch shim has children
//!   of its own, and `Child::kill` alone would leave them running.

use std::ffi::OsStr;
use std::fmt;
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// How often the watch loop checks cancel, the deadline and the child.
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// How much stderr is kept. The rest is read and dropped so the child never
/// blocks on a full pipe.
const STDERR_CAP: usize = 64 * 1024;

/// How long to wait, after the child exits, for its output pipes to close. A
/// grandchild that inherited the pipes can hold them open forever; past this
/// the remaining output is abandoned rather than hanging the caller.
const DRAIN_GRACE: Duration = Duration::from_secs(2);

/// A `Command` that never flashes a console window on Windows, and does not
/// leak the AppImage's bundled libraries into the child on Linux. Same policy
/// as `git_command()`, for any program.
pub fn hidden_command(program: impl AsRef<OsStr>) -> Command {
    #[allow(unused_mut)]
    let mut cmd = Command::new(program);
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW
    }
    #[cfg(target_os = "linux")]
    if std::env::var_os("APPDIR").is_some() {
        cmd.env_remove("LD_LIBRARY_PATH");
    }
    cmd
}

/// How a watched child ended, when it ended on its own.
#[derive(Debug, Clone)]
pub struct ExitInfo {
    /// The exit code, or `None` when the child was ended by a signal.
    pub code: Option<i32>,
    pub success: bool,
    /// The first [`STDERR_CAP`] bytes of stderr, lossily decoded.
    pub stderr: String,
}

#[derive(Debug)]
pub enum ProcessError {
    /// The child could not be started. `NotFound` means the program does not
    /// exist; on Windows, `InvalidInput` is also how std refuses arguments it
    /// cannot pass safely through a `.cmd`/`.bat` file.
    Spawn(std::io::Error),
    /// The cancel flag was raised; the process tree was killed.
    Cancelled,
    /// The deadline passed; the process tree was killed.
    TimedOut(Duration),
    /// Waiting on the child failed; the process tree was killed.
    Wait(std::io::Error),
}

impl fmt::Display for ProcessError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Spawn(e) => write!(f, "failed to start process: {e}"),
            Self::Cancelled => write!(f, "process cancelled"),
            Self::TimedOut(d) => write!(f, "process timed out after {}s", d.as_secs()),
            Self::Wait(e) => write!(f, "failed to wait for process: {e}"),
        }
    }
}

impl std::error::Error for ProcessError {}

/// Run `cmd` to completion under a cancel flag and a deadline.
///
/// `stdin_bytes`, when given, is written to the child's stdin from a separate
/// thread and stdin is then closed; otherwise stdin is null. Each stdout line
/// (without its line ending) is passed to `on_stdout_line` on the calling
/// thread as soon as it arrives. stderr is collected and returned.
///
/// On cancel or timeout the whole process tree is killed before returning:
/// `taskkill /T /F` on Windows, the child's own process group on Unix.
pub fn run_watched<F: FnMut(&str)>(
    mut cmd: Command,
    stdin_bytes: Option<Vec<u8>>,
    cancel: &Arc<AtomicBool>,
    timeout: Duration,
    mut on_stdout_line: F,
) -> Result<ExitInfo, ProcessError> {
    cmd.stdin(if stdin_bytes.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    })
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // Its own process group, so the whole tree can be killed at once.
        cmd.process_group(0);
    }

    let mut child = cmd.spawn().map_err(ProcessError::Spawn)?;

    if let (Some(bytes), Some(mut stdin)) = (stdin_bytes, child.stdin.take()) {
        // A thread, so a child that writes a lot before reading its input
        // cannot deadlock against us. Dropping `stdin` closes it.
        std::thread::spawn(move || {
            let _ = stdin.write_all(&bytes);
        });
    }

    let (line_tx, line_rx) = mpsc::channel::<String>();
    if let Some(stdout) = child.stdout.take() {
        std::thread::spawn(move || read_lines(stdout, line_tx));
    } else {
        drop(line_tx);
    }
    let (stderr_tx, stderr_rx) = mpsc::channel::<String>();
    if let Some(stderr) = child.stderr.take() {
        std::thread::spawn(move || {
            let _ = stderr_tx.send(read_capped(stderr, STDERR_CAP));
        });
    } else {
        drop(stderr_tx);
    }

    let deadline = Instant::now().checked_add(timeout);
    let mut stdout_open = true;

    let status = loop {
        if stdout_open {
            match line_rx.recv_timeout(POLL_INTERVAL) {
                Ok(line) => {
                    on_stdout_line(&line);
                    while let Ok(line) = line_rx.try_recv() {
                        on_stdout_line(&line);
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => stdout_open = false,
            }
        } else {
            std::thread::sleep(POLL_INTERVAL);
        }

        if cancel.load(Ordering::SeqCst) {
            kill_tree(&mut child);
            return Err(ProcessError::Cancelled);
        }

        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if deadline.is_some_and(|d| Instant::now() >= d) {
                    kill_tree(&mut child);
                    return Err(ProcessError::TimedOut(timeout));
                }
            }
            Err(e) => {
                kill_tree(&mut child);
                return Err(ProcessError::Wait(e));
            }
        }
    };

    // The child is gone; whatever it wrote is already in the pipe. Drain it,
    // but do not wait forever on a grandchild that kept the pipe open.
    let drain_deadline = Instant::now() + DRAIN_GRACE;
    while stdout_open {
        let now = Instant::now();
        if now >= drain_deadline {
            log::warn!("process exited but its stdout stayed open; remaining output dropped");
            break;
        }
        match line_rx.recv_timeout((drain_deadline - now).min(POLL_INTERVAL)) {
            Ok(line) => on_stdout_line(&line),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => stdout_open = false,
        }
    }
    let stderr_wait = drain_deadline
        .saturating_duration_since(Instant::now())
        .max(POLL_INTERVAL);
    let stderr = stderr_rx.recv_timeout(stderr_wait).unwrap_or_default();

    Ok(ExitInfo {
        code: status.code(),
        success: status.success(),
        stderr,
    })
}

fn read_lines(stdout: impl Read, tx: mpsc::Sender<String>) {
    let mut reader = BufReader::new(stdout);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match reader.read_until(b'\n', &mut buf) {
            Ok(0) | Err(_) => break,
            Ok(_) => {
                while matches!(buf.last(), Some(b'\n' | b'\r')) {
                    buf.pop();
                }
                if tx.send(String::from_utf8_lossy(&buf).into_owned()).is_err() {
                    break;
                }
            }
        }
    }
}

fn read_capped(mut source: impl Read, cap: usize) -> String {
    let mut kept = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        match source.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                let room = cap.saturating_sub(kept.len());
                kept.extend_from_slice(&chunk[..n.min(room)]);
            }
        }
    }
    String::from_utf8_lossy(&kept).into_owned()
}

/// Kill `child` and everything it started, then reap it.
///
/// The child is still unreaped when this runs, so its PID (and on Unix its
/// process-group ID) cannot have been reused by an unrelated process.
fn kill_tree(child: &mut Child) {
    #[cfg(windows)]
    {
        let system_root = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
        let taskkill = std::path::Path::new(&system_root)
            .join("System32")
            .join("taskkill.exe");
        let _ = hidden_command(taskkill)
            .args(["/PID", &child.id().to_string(), "/T", "/F"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    #[cfg(unix)]
    {
        extern "C" {
            fn kill(pid: i32, sig: i32) -> i32;
        }
        const SIGKILL: i32 = 9;
        if let Ok(pgid) = i32::try_from(child.id()) {
            // SAFETY: plain libc call; a negative PID addresses the process
            // group the child leads (it was spawned with process_group(0)).
            unsafe {
                kill(-pgid, SIGKILL);
            }
        }
    }
    // The direct child, in case the tree kill did not reach it.
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::Mutex;

    /// A shell running `script`: `cmd /D /S /C "<script>"` on Windows, `sh -c`
    /// elsewhere. The script reaches cmd verbatim (`raw_arg`), so it can carry
    /// its own quotes.
    pub(crate) fn shell(script: &str) -> Command {
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            let mut cmd = hidden_command("cmd");
            cmd.args(["/D", "/S", "/C"])
                .raw_arg(format!("\"{script}\""));
            cmd
        }
        #[cfg(not(windows))]
        {
            let mut cmd = hidden_command("sh");
            cmd.arg("-c").arg(script);
            cmd
        }
    }

    fn never() -> Arc<AtomicBool> {
        Arc::new(AtomicBool::new(false))
    }

    const PROBE_ENV: &str = "SKILLS_MANAGER_PROCESS_TREE_PROBE";

    /// Not a real test: the process-tree tests below run this binary again,
    /// filtered to this function, as a grandchild that reports its PID and
    /// then sleeps. Without the environment variable it does nothing.
    #[test]
    fn process_tree_probe() {
        if std::env::var_os(PROBE_ENV).is_none() {
            return;
        }
        println!("PROBE_PID={}", std::process::id());
        std::thread::sleep(Duration::from_secs(60));
    }

    /// A shell that starts the probe as its child, so the probe is the
    /// grandchild of whoever runs the shell. On Unix the trailing `; true`
    /// stops `sh` from exec'ing the probe in place of itself.
    fn shell_with_probe_grandchild() -> Command {
        let exe = std::env::current_exe().unwrap();
        let filter = concat!(module_path!(), "::process_tree_probe")
            .split_once("::")
            .unwrap()
            .1;
        #[cfg(windows)]
        let script = format!(
            "\"{}\" --exact {filter} --nocapture --test-threads=1",
            exe.display()
        );
        #[cfg(not(windows))]
        let script = format!(
            "'{}' --exact {filter} --nocapture --test-threads=1; true",
            exe.display()
        );
        let mut cmd = shell(&script);
        cmd.env(PROBE_ENV, "1");
        cmd
    }

    /// libtest prints "test <name> ... " without a newline before the test
    /// runs, so the marker can arrive mid-line.
    fn probe_pid(line: &str) -> Option<u32> {
        let (_, pid) = line.split_once("PROBE_PID=")?;
        pid.trim().parse().ok()
    }

    fn process_alive(pid: u32) -> bool {
        #[cfg(windows)]
        {
            let out = hidden_command("tasklist")
                .args(["/FI", &format!("PID eq {pid}"), "/NH", "/FO", "CSV"])
                .output()
                .unwrap();
            String::from_utf8_lossy(&out.stdout).contains(&format!("\"{pid}\""))
        }
        #[cfg(unix)]
        {
            extern "C" {
                fn kill(pid: i32, sig: i32) -> i32;
            }
            // SAFETY: signal 0 only checks that the PID exists.
            unsafe { kill(pid as i32, 0) == 0 }
        }
    }

    /// Wait briefly for `pid` to disappear: the kill is synchronous, but the
    /// OS may take a moment to tear the process down.
    fn gone_within(pid: u32, limit: Duration) -> bool {
        let until = Instant::now() + limit;
        while Instant::now() < until {
            if !process_alive(pid) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        !process_alive(pid)
    }

    #[test]
    fn stdout_lines_are_streamed_while_the_child_runs() {
        #[cfg(windows)]
        let script = "echo first& ping -n 3 127.0.0.1 >nul& echo second";
        #[cfg(not(windows))]
        let script = "echo first; sleep 2; echo second";

        let mut seen: Vec<(String, Instant)> = Vec::new();
        let exit = run_watched(
            shell(script),
            None,
            &never(),
            Duration::from_secs(30),
            |line| seen.push((line.to_string(), Instant::now())),
        )
        .unwrap();

        assert!(exit.success);
        let lines: Vec<&str> = seen.iter().map(|(l, _)| l.as_str()).collect();
        assert_eq!(lines, ["first", "second"]);
        // Delivered as they were written, not all at the end.
        let gap = seen[1].1.duration_since(seen[0].1);
        assert!(gap >= Duration::from_secs(1), "lines arrived {gap:?} apart");
    }

    #[test]
    fn stdin_is_fed_and_exit_code_and_stderr_are_reported() {
        #[cfg(windows)]
        let script = "sort& echo oops 1>&2& exit /b 3";
        #[cfg(not(windows))]
        let script = "sort; echo oops >&2; exit 3";

        let mut lines = Vec::new();
        let exit = run_watched(
            shell(script),
            Some(b"banana\ncherry\napple\n".to_vec()),
            &never(),
            Duration::from_secs(30),
            |line| lines.push(line.to_string()),
        )
        .unwrap();

        assert_eq!(lines, ["apple", "banana", "cherry"]);
        assert_eq!(exit.code, Some(3));
        assert!(!exit.success);
        assert_eq!(exit.stderr.trim(), "oops");
    }

    #[test]
    fn a_missing_program_is_a_spawn_error_not_a_hang() {
        let started = Instant::now();
        let err = run_watched(
            hidden_command("skills-manager-no-such-program-4f1c"),
            Some(b"ignored".to_vec()),
            &never(),
            Duration::from_secs(30),
            |_| {},
        )
        .unwrap_err();
        match err {
            ProcessError::Spawn(e) => assert_eq!(e.kind(), std::io::ErrorKind::NotFound),
            other => panic!("expected a spawn error, got {other:?}"),
        }
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn cancel_kills_the_whole_process_tree() {
        let cancel = never();
        let grandchild = Mutex::new(None);
        let started = Instant::now();
        let result = run_watched(
            shell_with_probe_grandchild(),
            None,
            &cancel,
            Duration::from_secs(120),
            |line| {
                if let Some(pid) = probe_pid(line) {
                    *grandchild.lock().unwrap() = Some(pid);
                    cancel.store(true, Ordering::SeqCst);
                }
            },
        );

        assert!(
            matches!(result, Err(ProcessError::Cancelled)),
            "expected Cancelled, got {result:?}"
        );
        // The probe sleeps 60 s; returning well before that means we did not
        // wait for it.
        assert!(started.elapsed() < Duration::from_secs(45));
        let pid = grandchild
            .lock()
            .unwrap()
            .expect("the probe never reported its PID");
        assert!(
            gone_within(pid, Duration::from_secs(5)),
            "grandchild {pid} survived the cancel"
        );
    }

    #[test]
    fn timeout_kills_the_whole_process_tree() {
        let grandchild = Mutex::new(None);
        let started = Instant::now();
        let result = run_watched(
            shell_with_probe_grandchild(),
            None,
            &never(),
            Duration::from_secs(8),
            |line| {
                if let Some(pid) = probe_pid(line) {
                    *grandchild.lock().unwrap() = Some(pid);
                }
            },
        );

        assert!(
            matches!(result, Err(ProcessError::TimedOut(d)) if d == Duration::from_secs(8)),
            "expected TimedOut, got {result:?}"
        );
        assert!(started.elapsed() < Duration::from_secs(45));
        let pid = grandchild
            .lock()
            .unwrap()
            .expect("the probe never reported its PID before the timeout");
        assert!(
            gone_within(pid, Duration::from_secs(5)),
            "grandchild {pid} survived the timeout"
        );
    }
}
