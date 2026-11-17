//! The tool runner: starts one program, collects its output within caps and kills it at a
//! deadline.
//!
//! [`Runner`] is the trait the stream resolver talks to and its tests fake. [`ProcessRunner`] is
//! the real one: `std::process::Command` with the program and each argument passed as they are
//! (no shell), stdin closed, and no console window on Windows. stdout is capped at 16 MiB and
//! stderr at 64 KiB; neither is ever logged, and no error carries either of them (hard rule 1:
//! a tool's stderr can hold a cookie or a URL).

use std::ffi::OsString;
use std::fmt;
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use crate::tools::os;

/// The most stdout kept: 16 MiB. More kills the process.
pub const STDOUT_CAP: usize = 16 << 20;
/// The most stderr kept: 64 KiB. The rest is read and thrown away so the process never blocks.
pub const STDERR_CAP: usize = 64 << 10;
/// How long, after the process has ended, the readers may take to finish the pipes. A process
/// the tool started can still hold a pipe open.
const READER_GRACE: Duration = Duration::from_secs(2);
const POLL: Duration = Duration::from_millis(10);

/// What a finished process left.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Output {
    /// The exit code; `None` when the OS gave none (a signal).
    pub code: Option<i32>,
    pub stdout: Vec<u8>,
    /// At most [`STDERR_CAP`] bytes. Never print it.
    pub stderr: Vec<u8>,
}

impl Output {
    /// Exit code 0.
    pub fn success(&self) -> bool {
        self.code == Some(0)
    }
}

/// Why a run gave no output. Fixed messages: nothing from the program, its arguments or its
/// streams.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunError {
    /// The program could not be started.
    Spawn,
    /// The program ran past its deadline and was killed.
    Timeout,
    /// The program wrote more than the stdout cap and was killed.
    OutputTooLarge,
    /// Waiting for the program failed.
    Io,
}

impl fmt::Display for RunError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            RunError::Spawn => "The helper could not be started",
            RunError::Timeout => "The helper took too long",
            RunError::OutputTooLarge => "The helper's output was too large",
            RunError::Io => "The helper could not be run",
        })
    }
}

impl std::error::Error for RunError {}

/// Runs a program to its end.
pub trait Runner: Send + Sync {
    /// Starts `program` with `args`, waits at most `timeout`, and returns what it printed.
    fn run(&self, program: &Path, args: &[OsString], timeout: Duration)
    -> Result<Output, RunError>;
}

/// What one reader thread has collected.
#[derive(Default)]
struct Pipe {
    data: Vec<u8>,
    done: bool,
    /// More than the cap arrived (only a stdout pipe stops there).
    overflow: bool,
}

type SharedPipe = Arc<Mutex<Pipe>>;

fn lock(p: &SharedPipe) -> std::sync::MutexGuard<'_, Pipe> {
    p.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Reads `source` on its own thread into a shared buffer. With `drain` the bytes past `cap` are
/// read and dropped; without it the reader stops at the first byte past `cap` and flags it.
fn spawn_reader(mut source: impl Read + Send + 'static, cap: usize, drain: bool) -> SharedPipe {
    let pipe: SharedPipe = Arc::default();
    let shared = Arc::clone(&pipe);
    thread::spawn(move || {
        let mut chunk = [0u8; 8192];
        loop {
            match source.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let mut p = lock(&shared);
                    let room = cap.saturating_sub(p.data.len());
                    if n > room {
                        p.data.extend_from_slice(&chunk[..room]);
                        if !drain {
                            p.overflow = true;
                            break;
                        }
                    } else {
                        p.data.extend_from_slice(&chunk[..n]);
                    }
                }
            }
        }
        lock(&shared).done = true;
    });
    pipe
}

/// The real [`Runner`]: a child process, no shell.
#[derive(Debug, Clone, Copy)]
pub struct ProcessRunner {
    stdout_cap: usize,
    stderr_cap: usize,
}

impl Default for ProcessRunner {
    fn default() -> Self {
        ProcessRunner {
            stdout_cap: STDOUT_CAP,
            stderr_cap: STDERR_CAP,
        }
    }
}

impl ProcessRunner {
    pub fn new() -> ProcessRunner {
        ProcessRunner::default()
    }

    /// A runner with other caps (tests use small ones).
    #[cfg(test)]
    fn with_caps(stdout_cap: usize, stderr_cap: usize) -> ProcessRunner {
        ProcessRunner {
            stdout_cap,
            stderr_cap,
        }
    }
}

impl Runner for ProcessRunner {
    fn run(
        &self,
        program: &Path,
        args: &[OsString],
        timeout: Duration,
    ) -> Result<Output, RunError> {
        let mut cmd = Command::new(program);
        cmd.args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        os::configure_command(&mut cmd);
        let mut child = cmd.spawn().map_err(|_| RunError::Spawn)?;

        let (Some(out), Some(err)) = (child.stdout.take(), child.stderr.take()) else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(RunError::Io);
        };
        let stdout = spawn_reader(out, self.stdout_cap, false);
        let stderr = spawn_reader(err, self.stderr_cap, true);

        let started = Instant::now();
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) => {}
                Err(_) => {
                    kill(&mut child);
                    return Err(RunError::Io);
                }
            }
            if lock(&stdout).overflow {
                kill(&mut child);
                return Err(RunError::OutputTooLarge);
            }
            if started.elapsed() >= timeout {
                kill(&mut child);
                return Err(RunError::Timeout);
            }
            thread::sleep(POLL);
        };

        // The process has ended; let the readers reach the end of their pipes.
        let grace = Instant::now();
        while !(lock(&stdout).done && lock(&stderr).done) && grace.elapsed() < READER_GRACE {
            thread::sleep(POLL);
        }
        if lock(&stdout).overflow {
            return Err(RunError::OutputTooLarge);
        }
        let stdout = std::mem::take(&mut lock(&stdout).data);
        let stderr = std::mem::take(&mut lock(&stderr).data);
        Ok(Output {
            code: status.code(),
            stdout,
            stderr,
        })
    }
}

/// Kills the process and what it started, then reaps it.
fn kill(child: &mut std::process::Child) {
    os::kill_tree(child.id());
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    // The tests below run a real process: this test binary itself, asked to run one of the
    // `helper_*` tests (ignored in a normal run). That gives a program with known output on every
    // OS, with no shell and no yt-dlp.

    fn helper(name: &str, extra: &[&str]) -> (std::path::PathBuf, Vec<OsString>) {
        let exe = std::env::current_exe().expect("test binary");
        let mut args: Vec<OsString> = ["--ignored", "--exact", "--nocapture"]
            .iter()
            .map(OsString::from)
            .collect();
        args.push(format!("run::tests::{name}").into());
        args.extend(extra.iter().map(OsString::from));
        (exe, args)
    }

    #[test]
    #[ignore = "helper: run as a child process by the runner tests"]
    fn helper_echo() {
        println!("HELLO-FROM-CHILD");
    }

    #[test]
    #[ignore = "helper: run as a child process by the runner tests"]
    fn helper_args() {
        let all: Vec<String> = std::env::args().collect();
        println!("ARGS<{}>", all.join("|"));
    }

    #[test]
    #[ignore = "helper: run as a child process by the runner tests"]
    fn helper_stdin() {
        let mut rest = Vec::new();
        std::io::stdin().read_to_end(&mut rest).unwrap();
        println!("STDIN-BYTES:{}", rest.len());
    }

    #[test]
    #[ignore = "helper: run as a child process by the runner tests"]
    fn helper_sleep() {
        thread::sleep(Duration::from_secs(20));
    }

    #[test]
    #[ignore = "helper: run as a child process by the runner tests"]
    fn helper_flood_stdout() {
        let mut out = std::io::stdout().lock();
        let line = [b'x'; 1000];
        for _ in 0..20 {
            let _ = out.write_all(&line);
        }
        let _ = out.flush();
    }

    #[test]
    #[ignore = "helper: run as a child process by the runner tests"]
    fn helper_flood_stderr() {
        let mut err = std::io::stderr().lock();
        let line = [b'e'; 1000];
        for _ in 0..200 {
            let _ = err.write_all(&line);
        }
        let _ = err.flush();
        println!("DONE-AFTER-STDERR");
    }

    fn text(b: &[u8]) -> String {
        String::from_utf8_lossy(b).into_owned()
    }

    #[test]
    fn it_collects_stdout_and_the_exit_code() {
        let (exe, args) = helper("helper_echo", &[]);
        let out = ProcessRunner::new()
            .run(&exe, &args, Duration::from_secs(60))
            .unwrap();
        assert!(out.success(), "{}", text(&out.stderr));
        assert!(text(&out.stdout).contains("HELLO-FROM-CHILD"));
    }

    #[test]
    fn arguments_reach_the_program_as_they_are_with_no_shell() {
        let payload = "& echo INJECTED && dir | more > x.txt";
        let (exe, args) = helper("helper_args", &[payload]);
        let out = ProcessRunner::new()
            .run(&exe, &args, Duration::from_secs(60))
            .unwrap();
        let out = text(&out.stdout);
        // The whole string arrives as one argument; a shell would have run the `echo`.
        assert!(out.contains(&format!("|{payload}>")), "{out}");
        assert!(!out.lines().any(|l| l.trim() == "INJECTED"), "{out}");
    }

    #[test]
    fn stdin_is_closed() {
        let (exe, args) = helper("helper_stdin", &[]);
        let started = Instant::now();
        let out = ProcessRunner::new()
            .run(&exe, &args, Duration::from_secs(60))
            .unwrap();
        assert!(
            text(&out.stdout).contains("STDIN-BYTES:0"),
            "{}",
            text(&out.stdout)
        );
        assert!(started.elapsed() < Duration::from_secs(30));
    }

    #[test]
    fn a_process_past_its_deadline_is_killed() {
        let (exe, args) = helper("helper_sleep", &[]);
        let started = Instant::now();
        let r = ProcessRunner::new().run(&exe, &args, Duration::from_millis(500));
        assert_eq!(r, Err(RunError::Timeout));
        // The helper sleeps 20 s: returning well before that means the wait was cut short.
        assert!(
            started.elapsed() < Duration::from_secs(15),
            "{:?}",
            started.elapsed()
        );
    }

    #[test]
    fn stdout_over_its_cap_kills_the_process() {
        let (exe, args) = helper("helper_flood_stdout", &[]);
        let r =
            ProcessRunner::with_caps(1024, STDERR_CAP).run(&exe, &args, Duration::from_secs(60));
        assert_eq!(r, Err(RunError::OutputTooLarge));
    }

    #[test]
    fn stderr_over_its_cap_is_cut_and_the_process_still_finishes() {
        let (exe, args) = helper("helper_flood_stderr", &[]);
        let out = ProcessRunner::with_caps(STDOUT_CAP, 1024)
            .run(&exe, &args, Duration::from_secs(60))
            .unwrap();
        assert!(out.success());
        assert_eq!(out.stderr.len(), 1024);
        assert!(text(&out.stdout).contains("DONE-AFTER-STDERR"));
    }

    #[test]
    fn a_program_that_does_not_exist_is_a_fixed_error() {
        let missing = Path::new("C:/no/such/dir/secret-token-tool");
        let err = ProcessRunner::new()
            .run(
                missing,
                &[OsString::from("--cookies=SECRET")],
                Duration::from_secs(5),
            )
            .unwrap_err();
        assert_eq!(err, RunError::Spawn);
        let shown = format!("{err} {err:?}");
        assert!(
            !shown.contains("secret") && !shown.contains("SECRET"),
            "{shown}"
        );
    }

    #[test]
    fn the_default_caps_are_the_plans() {
        assert_eq!(STDOUT_CAP, 16 * 1024 * 1024);
        assert_eq!(STDERR_CAP, 64 * 1024);
    }

    #[cfg(windows)]
    #[test]
    fn the_windows_runner_asks_for_no_console_window() {
        // `configure_command` sets exactly this flag; `Command` offers no getter to read it back.
        assert_eq!(os::CREATE_NO_WINDOW, 0x0800_0000);
    }
}
