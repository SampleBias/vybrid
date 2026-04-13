#![allow(dead_code)]

use anyhow::Result;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

/// Execute a bash command
pub async fn execute_bash(
    command: &str,
    description: Option<&str>,
    working_directory: Option<&str>,
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

    let mut child = cmd
        .spawn()
        .map_err(|e| anyhow::anyhow!("Failed to spawn command: {}", e))?;

    let stdout = child.stdout.take().expect("Failed to capture stdout");
    let stderr = child.stderr.take().expect("Failed to capture stderr");

    let mut stdout_reader = BufReader::new(stdout).lines();
    let mut stderr_reader = BufReader::new(stderr).lines();

    let mut output = String::new();
    let mut error_output = String::new();

    // Read stdout
    while let Some(line) = stdout_reader
        .next_line()
        .await
        .map_err(|e| anyhow::anyhow!("Failed to read stdout: {}", e))?
    {
        output.push_str(&line);
        output.push('\n');
    }

    // Read stderr
    while let Some(line) = stderr_reader
        .next_line()
        .await
        .map_err(|e| anyhow::anyhow!("Failed to read stderr: {}", e))?
    {
        error_output.push_str(&line);
        error_output.push('\n');
    }

    // Wait for the process to complete
    let status = child
        .wait()
        .await
        .map_err(|e| anyhow::anyhow!("Failed to wait for command: {}", e))?;

    let exit_code = status.code().unwrap_or(-1);

    // Build result
    let mut result = String::new();

    if !output.is_empty() {
        result.push_str("Output:\n");
        result.push_str(&output);
    }

    if !error_output.is_empty() {
        if !result.is_empty() {
            result.push_str("\n");
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
