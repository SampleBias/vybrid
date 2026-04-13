//! Structured `cargo` invocation for the agent (argv only; no shell).

use anyhow::{anyhow, Result};
use std::path::Path;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::time::timeout;

use super::file_ops::normalize_path;

/// Maximum combined stdout+stderr returned to the model (bytes).
pub const MAX_CARGO_OUTPUT_BYTES: usize = 256 * 1024;

fn subcommand_accepts_release(subcommand: &str) -> bool {
    matches!(
        subcommand,
        "build" | "check" | "test" | "run" | "bench" | "install" | "rustdoc"
    )
}

/// Build argv passed to the `cargo` executable (everything after `cargo` on the command line).
/// Used by tests and [`run_cargo`].
pub fn cargo_executable_args(
    subcommand: &str,
    release: bool,
    package: Option<&str>,
    manifest_path: Option<&str>,
    extra_args: &[String],
) -> Vec<String> {
    let mut args = vec!["--color".to_string(), "never".to_string()];
    args.push(subcommand.to_string());
    if release && subcommand_accepts_release(subcommand) {
        args.push("--release".to_string());
    }
    if let Some(p) = package {
        if !p.is_empty() {
            args.push("-p".to_string());
            args.push(p.to_string());
        }
    }
    if let Some(m) = manifest_path {
        if !m.is_empty() {
            args.push("--manifest-path".to_string());
            args.push(m.to_string());
        }
    }
    args.extend(extra_args.iter().cloned());
    args
}

fn default_timeout(subcommand: &str) -> Duration {
    match subcommand {
        "check" | "metadata" => Duration::from_secs(300),
        "test" | "bench" => Duration::from_secs(900),
        _ => Duration::from_secs(600),
    }
}

fn truncate_combined_output(combined: &str, max_bytes: usize) -> String {
    if combined.len() <= max_bytes {
        return combined.to_string();
    }
    let skip = combined.len().saturating_sub(max_bytes);
    let after = &combined[skip..];
    let rest = if let Some(pos) = after.find('\n') {
        &after[pos + 1..]
    } else {
        after
    };
    format!(
        "[Output truncated: {} bytes omitted; showing tail]\n\n{}",
        skip, rest
    )
}

/// Run `cargo` with structured arguments; captures stdout and stderr, waits for exit.
pub async fn run_cargo(
    subcommand: &str,
    release: bool,
    package: Option<&str>,
    manifest_path: Option<&str>,
    extra_args: &[String],
    working_directory: Option<&str>,
) -> Result<String> {
    if subcommand.trim().is_empty() {
        return Err(anyhow!(
            "run_cargo: subcommand is required (e.g. check, build, test)"
        ));
    }

    let argv = cargo_executable_args(subcommand, release, package, manifest_path, extra_args);
    let dur = default_timeout(subcommand);

    let mut cmd = Command::new("cargo");
    cmd.args(&argv);
    cmd.env("CARGO_TERM_COLOR", "never");
    cmd.env("TERM", "dumb");
    cmd.kill_on_drop(true);

    if let Some(dir) = working_directory {
        let normalized = normalize_path(dir);
        if !Path::new(&normalized).exists() {
            return Err(anyhow!(
                "run_cargo: working_directory does not exist: {}",
                normalized
            ));
        }
        cmd.current_dir(&normalized);
    }

    let run = async {
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());

        let mut child = cmd
            .spawn()
            .map_err(|e| anyhow!("run_cargo: failed to spawn cargo: {}", e))?;

        let stdout = child.stdout.take().expect("stdout piped");
        let stderr = child.stderr.take().expect("stderr piped");

        let mut stdout_reader = BufReader::new(stdout).lines();
        let mut stderr_reader = BufReader::new(stderr).lines();

        let mut out = String::new();
        while let Some(line) = stdout_reader
            .next_line()
            .await
            .map_err(|e| anyhow!("run_cargo: read stdout: {}", e))?
        {
            out.push_str(&line);
            out.push('\n');
        }
        let mut err = String::new();
        while let Some(line) = stderr_reader
            .next_line()
            .await
            .map_err(|e| anyhow!("run_cargo: read stderr: {}", e))?
        {
            err.push_str(&line);
            err.push('\n');
        }

        let status = child
            .wait()
            .await
            .map_err(|e| anyhow!("run_cargo: wait: {}", e))?;

        Ok::<_, anyhow::Error>((out, err, status))
    };

    let result = timeout(dur, run).await;

    let (output, error_output, status) = match result {
        Ok(Ok(triple)) => triple,
        Ok(Err(e)) => return Err(e),
        Err(_elapsed) => {
            return Err(anyhow!(
                "run_cargo: timed out after {:?} (subcommand: {})",
                dur,
                subcommand
            ));
        }
    };

    let exit_code = status.code().unwrap_or(-1);
    let mut combined = String::new();
    if !output.is_empty() {
        combined.push_str("Output:\n");
        combined.push_str(&output);
    }
    if !error_output.is_empty() {
        if !combined.is_empty() {
            combined.push('\n');
        }
        combined.push_str("Stderr:\n");
        combined.push_str(&error_output);
    }
    if combined.is_empty() {
        combined = format!("Command completed with exit code {}", exit_code);
    } else {
        combined.push_str(&format!("\nExit code: {}", exit_code));
    }

    combined = truncate_combined_output(&combined, MAX_CARGO_OUTPUT_BYTES);

    if !status.success() {
        combined = format!("Command failed (exit code {})\n{}", exit_code, combined);
    }

    Ok(combined)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cargo_argv_check_minimal() {
        let a = cargo_executable_args("check", false, None, None, &[]);
        assert_eq!(
            a,
            vec!["--color", "never", "check"]
                .into_iter()
                .map(String::from)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn cargo_argv_release_and_package() {
        let a = cargo_executable_args("test", true, Some("my-crate"), None, &[]);
        assert_eq!(
            a,
            vec!["--color", "never", "test", "--release", "-p", "my-crate",]
                .into_iter()
                .map(String::from)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn cargo_argv_manifest_and_extra() {
        let extra = vec!["--no-run".into(), "--".into(), "nocapture".into()];
        let a = cargo_executable_args("test", false, None, Some("/tmp/proj/Cargo.toml"), &extra);
        assert!(a.iter().any(|s| s == "--manifest-path"));
        assert!(a.iter().any(|s| s == "/tmp/proj/Cargo.toml"));
        let n = a.len();
        assert_eq!(&a[n - 3..], &extra[..]);
    }

    #[test]
    fn fmt_skips_release() {
        let a = cargo_executable_args("fmt", true, None, None, &[]);
        assert_eq!(
            a,
            vec!["--color", "never", "fmt"]
                .into_iter()
                .map(String::from)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn truncate_keeps_tail() {
        let s: String = (0..1000).map(|_| 'x').collect();
        let t = truncate_combined_output(&s, 100);
        assert!(t.contains("truncated"));
        assert!(t.len() <= s.len());
        assert!(t.ends_with('x'));
    }
}
