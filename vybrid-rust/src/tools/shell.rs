#![allow(dead_code)]

use anyhow::Result;
use std::process::Stdio;
use tokio::process::Command;
use tokio::time::{timeout, Duration};

const DEFAULT_SHELL_TIMEOUT_SECS: u64 = 300;
const MAX_SHELL_OUTPUT_BYTES: usize = 96 * 1024;

fn truncate_output(output: &str, max_bytes: usize) -> String {
    super::output::truncate_utf8_middle(output, max_bytes, "Shell output")
}

/// Execute a bash command
pub async fn execute_bash(
    command: &str,
    description: Option<&str>,
    working_directory: Option<&str>,
) -> Result<String> {
    execute_bash_with_store(
        command,
        description,
        working_directory,
        &super::output::ToolOutputStore::default(),
    )
    .await
}

pub async fn execute_bash_with_store(
    command: &str,
    description: Option<&str>,
    working_directory: Option<&str>,
    store: &super::output::ToolOutputStore,
) -> Result<String> {
    if let Some(desc) = description {
        eprintln!("Executing: {} ({})", command, desc);
    } else {
        eprintln!("Executing: {}", command);
    }

    let mut cmd = Command::new("bash");
    cmd.arg("-c").arg(command);

    // Set working directory if provided
    if let Some(dir) = working_directory {
        let normalized = super::file_ops::normalize_path(dir);
        cmd.current_dir(&normalized);
    }

    // Capture stdout and stderr
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    cmd.kill_on_drop(true);

    let run = async {
        let mut child = cmd
            .spawn()
            .map_err(|e| anyhow::anyhow!("Failed to spawn command: {}", e))?;

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow::anyhow!("Failed to capture stdout"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| anyhow::anyhow!("Failed to capture stderr"))?;

        let (output, error_output, status) = tokio::try_join!(
            super::output::capture_pipe(stdout, store.clone(), "stdout", false),
            super::output::capture_pipe(stderr, store.clone(), "stderr", false),
            async { child.wait().await.map_err(anyhow::Error::from) }
        )?;
        Ok::<_, anyhow::Error>((output.text, error_output.text, status))
    };

    let (output, error_output, status) =
        timeout(Duration::from_secs(DEFAULT_SHELL_TIMEOUT_SECS), run)
            .await
            .map_err(|_| {
                anyhow::anyhow!(
                    "Command timed out after {}s: {}",
                    DEFAULT_SHELL_TIMEOUT_SECS,
                    command
                )
            })??;

    let exit_code = status.code().unwrap_or(-1);

    // Build result
    let mut result = String::new();

    if !output.is_empty() {
        result.push_str("Output:\n");
        result.push_str(&output);
    }

    if !error_output.is_empty() {
        if !result.is_empty() {
            result.push('\n');
        }
        result.push_str("Stderr:\n");
        result.push_str(&error_output);
    }

    if result.is_empty() {
        result = format!("Command completed with exit code {}", exit_code);
    } else {
        result.push_str(&format!("\nExit code: {}", exit_code));
    }

    if !status.success() {
        result = format!("Command failed (exit code {})\n{}", exit_code, result);
    }

    Ok(result)
}

/// Execute a simple command synchronously (for quick operations)
pub fn execute_bash_sync(command: &str) -> Result<String> {
    use std::process::Command as StdCommand;

    let output = StdCommand::new("bash")
        .arg("-c")
        .arg(command)
        .output()
        .map_err(|e| anyhow::anyhow!("Failed to execute command: {}", e))?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    let mut result = String::new();

    if !stdout.is_empty() {
        result.push_str(&stdout);
    }

    if !stderr.is_empty() {
        if !result.is_empty() {
            result.push('\n');
        }
        result.push_str(&stderr);
    }

    if result.is_empty() {
        result = format!(
            "Command completed with exit code {}",
            output.status.code().unwrap_or(-1)
        );
    }

    Ok(result)
}
