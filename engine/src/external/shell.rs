//! # Shell Command Execution
//!
//! Executes shell commands in the agent's workspace directory (async).
//! Provides timeout protection and output truncation to prevent OOM.
//!
//! Uses tokio::process::Command for async subprocess execution.
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

use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Result};

use thiserror::Error;
use tracing::{instrument, warn};

// ---------------------------------------------------------------------------
// Path Resolution
// ---------------------------------------------------------------------------

/// Resolve an action path to an absolute path.
/// Absolute paths are used directly (only meaningful for privileged instances;
/// sandboxed instances lack filesystem permissions outside workspace).
/// Relative paths are resolved within the workspace (rejects path traversal).
pub fn resolve_action_path(workspace: &Path, path: &str) -> Result<PathBuf> {
    if path.starts_with('/') {
        Ok(PathBuf::from(path))
    } else {
        let p = Path::new(path);
        for component in p.components() {
            if let Component::ParentDir = component {
                bail!("Path traversal rejected: {}", path);
            }
        }
        Ok(workspace.join(p))
    }
}

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

impl ShellOutput {
    /// Whether the command exited successfully (exit code 0).
    pub fn success(&self) -> bool {
        self.exit_code == Some(0)
    }

    /// Human-readable exit code for error messages.
    pub fn exit_code_display(&self) -> String {
        self.exit_code
            .map_or("unknown".to_string(), |c| c.to_string())
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Async read from `reader` into `buf` until EOF or `limit` bytes accumulated.
/// Uses chunked reads to avoid closing the pipe prematurely (SIGPIPE fix).
async fn read_limited_async(
    reader: &mut (impl tokio::io::AsyncRead + Unpin),
    limit: usize,
    buf: &mut Vec<u8>,
) {
    use tokio::io::AsyncReadExt;
    let mut chunk = [0u8; 8192];
    while buf.len() < limit {
        let remaining = limit - buf.len();
        let to_read = remaining.min(chunk.len());
        match reader.read(&mut chunk[..to_read]).await {
            Ok(0) => break, // EOF
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
            Err(_) => break, // I/O error, stop reading
        }
    }
}

// ---------------------------------------------------------------------------
// Shell
// ---------------------------------------------------------------------------

/// Shell command executor with timeout and truncation protection.
///
/// Both privileged and sandbox modes pipe scripts via stdin to bash,
/// ensuring identical I/O behavior and test coverage.
///
/// - No sandbox_user (privileged): `/bin/bash` with `current_dir`
/// - With sandbox_user: `su - {user} -c "bash -s"` (紧箍咒)
///
/// @TRACE: SHELL
pub struct Shell {
    working_dir: PathBuf,
    timeout_duration: Duration,
    max_output: usize,
    /// Linux username for sandboxed execution (紧箍咒)
    sandbox_user: Option<String>,
}

impl Shell {
    pub fn new(working_dir: PathBuf, sandbox_user: Option<String>) -> Self {
        Self {
            working_dir,
            timeout_duration: DEFAULT_TIMEOUT,
            max_output: MAX_OUTPUT_BYTES,
            sandbox_user,
        }
    }

    /// Detect whether a sandbox user exists for the given instance ID.
    /// Returns `Some("agent-{id}")` if the system user exists, `None` otherwise.
    pub fn detect_sandbox_user(instance_id: &str) -> Option<String> {
        let user = format!("agent-{}", instance_id);
        let exists = std::process::Command::new("id")
            .arg(&user)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if exists {
            Some(user)
        } else {
            None
        }
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
    pub async fn exec(&self, script: &str) -> ShellResult<ShellOutput> {
        self.exec_in_dir(script, &self.working_dir).await
    }

    /// Execute a shell script in a specific directory (async).
    ///
    /// Both paths pipe the script via stdin to bash, ensuring identical I/O behavior.
    /// Sandboxed mode wraps with `su -` for user isolation (紧箍咒).
    ///
    /// @TRACE: SHELL — `[SHELL-{id}] Executing in {dir}`
    #[instrument(skip(self, script), fields(trace = "SHELL"))]
    pub async fn exec_in_dir(&self, script: &str, dir: &Path) -> ShellResult<ShellOutput> {
        use tokio::process::Command;
        use std::process::Stdio;

        let start = std::time::Instant::now();
        let max_output = self.max_output;
        let timeout = self.timeout_duration;

        // Both paths use stdin to pipe the script to bash.
        let mut child = if self.sandbox_user.is_some() {
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
            let mut cmd = Command::new("/bin/bash");
            cmd.current_dir(dir)
                .env("ALICE_ENGINE_CHILD", "1")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            cmd.spawn()?
        };

        // Write script to stdin
        {
            let script_content = if self.sandbox_user.is_some() {
                format!("cd {} && {}", dir.display(), script)
            } else {
                script.to_string()
            };
            if let Some(mut stdin) = child.stdin.take() {
                use tokio::io::AsyncWriteExt;
                let _ = stdin.write_all(script_content.as_bytes()).await;
                // stdin drops here, closing the pipe — signals EOF to bash
            }
        }

        // Async read with timeout
        let read_and_wait = async {
            let mut combined = Vec::new();

            if let Some(mut stdout) = child.stdout.take() {
                read_limited_async(&mut stdout, max_output, &mut combined).await;
            }

            if let Some(mut stderr) = child.stderr.take() {
                let remaining = max_output.saturating_sub(combined.len());
                if remaining > 0 {
                    read_limited_async(&mut stderr, remaining, &mut combined).await;
                }
            }

            let status = child.wait().await;
            (combined, status)
        };

        match tokio::time::timeout(timeout, read_and_wait).await {
            Ok((raw_output, status)) => {
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
                warn!("[SHELL] Command timed out after {:?}", timeout);
                // Try to kill the child process
                let _ = child.kill().await;
                Err(ShellError::Timeout(timeout))
            }
        }
    }

    /// Read a file via shell `cat` command.
    /// Useful for sandboxed instances that lack direct filesystem access.
    pub async fn read_file(&self, path: &str) -> ShellResult<ShellOutput> {
        let escaped = path.replace('\'', "'\\''");
        self.exec(&format!("cat '{}'", escaped)).await
    }

    /// Write content to a file via shell heredoc.
    /// Creates parent directories if needed.
    pub async fn write_file(&self, path: &str, content: &str) -> ShellResult<ShellOutput> {
        let escaped = path.replace('\'', "'\\''");
        let delim = format!(
            "HEREDOC_{}",
            uuid::Uuid::new_v4().to_string().replace('-', "")
        );
        self.exec(&format!(
            "mkdir -p \"$(dirname '{}')\" && cat > '{}' << '{}'\n{}\n{}",
            escaped, escaped, delim, content, delim,
        )).await
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
        let shell = Shell::new(tmp.path().to_path_buf(), None);
        (shell, tmp)
    }

    #[tokio::test]
    async fn test_echo() {
        let (shell, _tmp) = setup();
        let result = shell.exec("echo hello").await.unwrap();
        assert_eq!(result.output.trim(), "hello");
        assert_eq!(result.exit_code, Some(0));
        assert!(!result.truncated);
    }

    #[tokio::test]
    async fn test_exit_code() {
        let (shell, _tmp) = setup();
        let result = shell.exec("exit 42").await.unwrap();
        assert_eq!(result.exit_code, Some(42));
    }

    #[tokio::test]
    async fn test_stderr_captured() {
        let (shell, _tmp) = setup();
        let result = shell.exec("echo err >&2").await.unwrap();
        assert!(result.output.contains("err"));
    }

    #[tokio::test]
    async fn test_working_dir() {
        let (shell, tmp) = setup();
        std::fs::write(tmp.path().join("test.txt"), "content").unwrap();
        let result = shell.exec("cat test.txt").await.unwrap();
        assert_eq!(result.output.trim(), "content");
    }

    #[tokio::test]
    async fn test_timeout() {
        let tmp = TempDir::new().unwrap();
        let shell =
            Shell::new(tmp.path().to_path_buf(), None).with_timeout(Duration::from_millis(500));
        let result = shell.exec("sleep 10").await;
        assert!(matches!(result, Err(ShellError::Timeout(_))));
    }

    #[tokio::test]
    async fn test_multiline_script() {
        let (shell, _tmp) = setup();
        let result = shell.exec("echo line1\necho line2").await.unwrap();
        assert!(result.output.contains("line1"));
        assert!(result.output.contains("line2"));
    }
}

// ─── System shell utilities ─────────────────────────────────────

/// Check available disk space at the given path. Returns available MB.
/// Wraps `df -BM --output=avail`.
pub fn available_mb(path: &std::path::Path) -> Option<u64> {
    let output = std::process::Command::new("df")
        .arg("-BM")
        .arg("--output=avail")
        .arg(path)
        .output()
        .ok()?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout
        .lines()
        .nth(1)?
        .trim()
        .trim_end_matches('M')
        .parse::<u64>()
        .ok()
}

/// Ensure a system user exists, creating if needed. Sets workspace ownership.
///
/// - Checks if user exists via `id {prefix}{name}`
/// - Creates user if missing via `useradd -r -s /bin/bash --home-dir {workspace} {prefix}{name}`
/// - Sets workspace directory ownership via `chown -R {user}:{user} {workspace}`
pub fn ensure_sandbox_user(
    prefix: &str,
    name: &str,
    workspace: &std::path::Path,
) -> anyhow::Result<()> {
    use anyhow::Context;

    let user = format!("{}{}", prefix, name);
    let workspace_str = workspace.to_string_lossy();

    // Check if user already exists
    let check = std::process::Command::new("id")
        .arg(&user)
        .output()
        .context("Failed to run 'id' command")?;

    if !check.status.success() {
        tracing::info!(
            "[SANDBOX] Creating sandbox user: {} (home={})",
            user,
            workspace_str
        );
        let create = std::process::Command::new("useradd")
            .args(["-r", "-s", "/bin/bash", "--home-dir", &workspace_str, &user])
            .output()
            .context("Failed to run 'useradd' command")?;

        if !create.status.success() {
            let stderr = String::from_utf8_lossy(&create.stderr);
            anyhow::bail!(
                "Failed to create sandbox user '{}': {}",
                user,
                stderr.trim()
            );
        }
        tracing::info!("[SANDBOX] Created sandbox user: {}", user);
    }

    // Ensure workspace ownership (user:user so group matches)
    let owner = format!("{}:{}", user, user);
    let chown = std::process::Command::new("chown")
        .args(["-R", &owner, &workspace_str])
        .output()
        .context("Failed to run 'chown' command")?;

    if !chown.status.success() {
        let stderr = String::from_utf8_lossy(&chown.stderr);
        tracing::warn!("[SANDBOX] chown failed for {}: {}", user, stderr.trim());
    }

    Ok(())
}

#[cfg(test)]
mod system_tests {
    // System utility tests require root and are not run in CI.
    // available_mb and ensure_sandbox_user are integration-tested manually.
}

#[cfg(test)]
mod truncation_tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_truncation() {
        let tmp = TempDir::new().unwrap();
        let shell = Shell::new(tmp.path().to_path_buf(), None).with_max_output(100);
        // Generate output larger than 100 bytes
        let result = shell.exec("yes | head -200").await.unwrap();
        assert!(result.truncated);
        assert!(result.output.len() <= 200); // some slack for encoding
    }
}