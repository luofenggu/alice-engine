//! # Shell Command Execution
//!
//! Executes shell commands in the agent's workspace directory (synchronous).
//! Provides timeout protection and output truncation to prevent OOM.
//!
//! Uses std::process::Command (blocking) because action execution is sequential.
//! Async is reserved for LLM inference (Phase 3).
//!
//! @TRACE: SHELL — All command executions are traced under SHELL line.
//!
//! ## Safety Design (双层截断)
//!
//! - **Shell layer**: Truncates output at 1MB to prevent OOM
//! - **Memory layer**: Further truncates at 100KB when recording to session
//!   (handled by Transaction, not here)
//!
//! The shell layer is the first line of defense. A runaway `cat` on a huge file
//! will be cut off here before it can bloat the process memory.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;
use std::thread;
use std::sync::mpsc;

use thiserror::Error;
use tracing::{warn, instrument};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Maximum output size in bytes (1MB). Prevents OOM from runaway commands.
const MAX_OUTPUT_BYTES: usize = 1_048_576;

/// Default command timeout.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(300);

// ---------------------------------------------------------------------------
// Error types
// ---------------------------------------------------------------------------

#[derive(Debug, Error)]
pub enum ShellError {
    #[error("Shell command timed out after {0:?}")]
    Timeout(Duration),

    #[error("Shell I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Shell output not valid UTF-8: {0}")]
    Utf8(#[from] std::string::FromUtf8Error),
}

pub type ShellResult<T> = std::result::Result<T, ShellError>;

// ---------------------------------------------------------------------------
// Output
// ---------------------------------------------------------------------------

/// Result of a shell command execution.
#[derive(Debug, Clone)]
pub struct ShellOutput {
    /// Combined stdout + stderr output (truncated if over limit).
    pub output: String,
    /// Process exit code (None if killed/timeout).
    pub exit_code: Option<i32>,
    /// Whether the output was truncated.
    pub truncated: bool,
    /// Execution time.
    pub duration: Duration,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Read from `reader` into `buf` until EOF or `limit` bytes accumulated.
/// Uses chunked reads to avoid closing the pipe prematurely (SIGPIPE fix).
fn read_limited(reader: &mut impl Read, limit: usize, buf: &mut Vec<u8>) {
    let mut chunk = [0u8; 8192];
    while buf.len() < limit {
        let remaining = limit - buf.len();
        let to_read = remaining.min(chunk.len());
        match reader.read(&mut chunk[..to_read]) {
            Ok(0) => break,       // EOF
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
            Err(_) => break,      // I/O error, stop reading
        }
    }
}

// ---------------------------------------------------------------------------
// Shell
// ---------------------------------------------------------------------------

/// Shell command executor with timeout and truncation protection.
///
/// Both privileged and sandboxed modes pipe scripts via stdin to bash,
/// ensuring identical I/O behavior and test coverage.
///
/// - `sandboxed=false` (privileged): `/bin/bash` with `current_dir`
/// - `sandboxed=true`: `su - {user} -c "bash -s"` (紧箍咒)
///
/// @TRACE: SHELL
pub struct Shell {
    working_dir: PathBuf,
    timeout_duration: Duration,
    max_output: usize,
    /// Whether to sandbox commands via su (紧箍咒)
    sandboxed: bool,
    /// Linux username for sandboxed execution
    sandbox_user: Option<String>,
}

impl Shell {
    pub fn new(working_dir: PathBuf) -> Self {
        Self {
            working_dir,
            timeout_duration: DEFAULT_TIMEOUT,
            max_output: MAX_OUTPUT_BYTES,
            sandboxed: false,
            sandbox_user: None,
        }
    }

    /// Enable sandboxing with a specific Linux user (紧箍咒).
    pub fn with_sandbox(mut self, user: String) -> Self {
        self.sandboxed = true;
        self.sandbox_user = Some(user);
        self
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout_duration = timeout;
        self
    }

    pub fn with_max_output(mut self, max: usize) -> Self {
        self.max_output = max;
        self
    }

    /// Execute a shell script in the configured working directory.
    ///
    /// @TRACE: SHELL — `[SHELL-{id}] Executing script ({n} bytes)`
    #[instrument(skip(self, script), fields(trace = "SHELL"))]
    pub fn exec(&self, script: &str) -> ShellResult<ShellOutput> {
        self.exec_in_dir(script, &self.working_dir)
    }

    /// Execute a shell script in a specific directory.
    ///
    /// Both paths pipe the script via stdin to bash, ensuring identical I/O behavior.
    /// Sandboxed mode wraps with `su -` for user isolation (紧箍咒).
    ///
    /// @TRACE: SHELL — `[SHELL-{id}] Executing in {dir}`
    #[instrument(skip(self, script), fields(trace = "SHELL"))]
    pub fn exec_in_dir(&self, script: &str, dir: &Path) -> ShellResult<ShellOutput> {
        let start = std::time::Instant::now();
        let max_output = self.max_output;
        let timeout = self.timeout_duration;

        // Both paths use stdin to pipe the script to bash.
        // This ensures identical I/O behavior (read_limited on stdout/stderr)
        // regardless of privilege mode, eliminating test coverage blind spots.
        let mut child = if self.sandboxed {
            // 紧箍咒: su降权执行
            let user = self.sandbox_user.as_deref().unwrap_or("nobody");
            let mut cmd = Command::new("su");
            cmd.arg("-")
                .arg(user)
                .arg("-c")
                .arg("bash -s")
                .env("ALICE_ENGINE_CHILD", "1")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            cmd.spawn()?
        } else {
            // Privileged: direct execution (same stdin-based approach)
            let mut cmd = Command::new("/bin/bash");
            cmd.current_dir(dir)
                .env("ALICE_ENGINE_CHILD", "1")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            cmd.spawn()?
        };

        // Unified: write script to stdin for both paths
        {
            let script_content = if self.sandboxed {
                format!("cd {} && {}", dir.display(), script)
            } else {
                script.to_string()
            };
            if let Some(mut stdin) = child.stdin.take() {
                use std::io::Write;
                let _ = stdin.write_all(script_content.as_bytes());
                // stdin drops here, closing the pipe — signals EOF to bash
            }
        }

        // Use a channel + thread for timeout
        let (tx, rx) = mpsc::channel();

        let child_stdout = child.stdout.take();
        let child_stderr = child.stderr.take();

        let handle = thread::spawn(move || {
            let mut combined = Vec::new();

            // Read stdout fully (loop until EOF), respecting max_output limit.
            // A single read() call may return partial data, causing the pipe to
            // close early and SIGPIPE the child process (Broken pipe bug).
            if let Some(mut stdout) = child_stdout {
                read_limited(&mut stdout, max_output, &mut combined);
            }

            // Read stderr with remaining budget
            if let Some(mut stderr) = child_stderr {
                let remaining = max_output.saturating_sub(combined.len());
                if remaining > 0 {
                    read_limited(&mut stderr, remaining, &mut combined);
                }
            }

            let status = child.wait();
            let _ = tx.send((combined, status));
        });

        match rx.recv_timeout(timeout) {
            Ok((raw_output, status)) => {
                let _ = handle.join();
                let truncated = raw_output.len() >= max_output;
                let output = String::from_utf8_lossy(&raw_output).to_string();
                let exit_code = status.ok().and_then(|s| s.code());
                let duration = start.elapsed();

                if truncated {
                    warn!("[SHELL] Output truncated at {} bytes", max_output);
                }

                Ok(ShellOutput {
                    output,
                    exit_code,
                    truncated,
                    duration,
                })
            }
            Err(_) => {
                // Timeout — try to kill the process
                warn!("[SHELL] Command timed out after {:?}", timeout);
                // The child is owned by the thread, we can't kill it directly.
                // The thread will eventually finish when the child exits.
                // For now, return timeout error.
                Err(ShellError::Timeout(timeout))
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn setup() -> (Shell, TempDir) {
        let tmp = TempDir::new().unwrap();
        let shell = Shell::new(tmp.path().to_path_buf());
        (shell, tmp)
    }

    #[test]
    fn test_echo() {
        let (shell, _tmp) = setup();
        let result = shell.exec("echo hello").unwrap();
        assert_eq!(result.output.trim(), "hello");
        assert_eq!(result.exit_code, Some(0));
        assert!(!result.truncated);
    }

    #[test]
    fn test_exit_code() {
        let (shell, _tmp) = setup();
        let result = shell.exec("exit 42").unwrap();
        assert_eq!(result.exit_code, Some(42));
    }

    #[test]
    fn test_stderr_captured() {
        let (shell, _tmp) = setup();
        let result = shell.exec("echo err >&2").unwrap();
        assert!(result.output.contains("err"));
    }

    #[test]
    fn test_working_dir() {
        let (shell, tmp) = setup();
        std::fs::write(tmp.path().join("test.txt"), "content").unwrap();
        let result = shell.exec("cat test.txt").unwrap();
        assert_eq!(result.output.trim(), "content");
    }

    #[test]
    fn test_timeout() {
        let tmp = TempDir::new().unwrap();
        let shell = Shell::new(tmp.path().to_path_buf())
            .with_timeout(Duration::from_millis(500));
        let result = shell.exec("sleep 10");
        assert!(matches!(result, Err(ShellError::Timeout(_))));
    }

    #[test]
    fn test_multiline_script() {
        let (shell, _tmp) = setup();
        let result = shell.exec("echo line1\necho line2").unwrap();
        assert!(result.output.contains("line1"));
        assert!(result.output.contains("line2"));
    }

    #[test]
    fn test_truncation() {
        let tmp = TempDir::new().unwrap();
        let shell = Shell::new(tmp.path().to_path_buf())
            .with_max_output(100);
        // Generate output larger than 100 bytes
        let result = shell.exec("yes | head -200").unwrap();
        assert!(result.truncated);
        assert!(result.output.len() <= 200); // some slack for encoding
    }
}