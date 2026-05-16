//! Structured `cargo` invocation for the agent (argv only; no shell).

use anyhow::{anyhow, Context, Result};
use serde_json::Value;
use std::collections::BTreeSet;
use std::path::Path;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::process::Command;
use tokio::time::timeout;

use super::file_ops::normalize_path;

/// Maximum combined stdout+stderr returned to the model (bytes).
pub const MAX_CARGO_OUTPUT_BYTES: usize = 128 * 1024;

/// How `run_cargo` should ask Cargo/rustc to format compiler diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagnosticFormat {
    Human,
    Json,
}

impl DiagnosticFormat {
    pub fn parse(value: Option<&str>) -> Self {
        match value
            .unwrap_or("human")
            .trim()
            .to_ascii_lowercase()
            .as_str()
        {
            "json" | "json-diagnostic" | "json-diagnostics" => Self::Json,
            _ => Self::Human,
        }
    }
}

fn subcommand_accepts_release(subcommand: &str) -> bool {
    matches!(
        subcommand,
        "build" | "check" | "test" | "run" | "bench" | "install" | "rustdoc"
    )
}

fn subcommand_accepts_json_diagnostics(subcommand: &str) -> bool {
    matches!(
        subcommand,
        "build" | "check" | "clippy" | "doc" | "rustdoc" | "test"
    )
}

/// Build argv passed to the `cargo` executable (everything after `cargo` on the command line).
/// Used by tests and [`run_cargo`].
#[allow(dead_code)]
pub fn cargo_executable_args(
    subcommand: &str,
    release: bool,
    package: Option<&str>,
    manifest_path: Option<&str>,
    extra_args: &[String],
) -> Vec<String> {
    cargo_executable_args_with_diagnostics(
        subcommand,
        release,
        package,
        manifest_path,
        extra_args,
        DiagnosticFormat::Human,
    )
}

/// Build argv with optional machine-readable compiler diagnostics.
pub fn cargo_executable_args_with_diagnostics(
    subcommand: &str,
    release: bool,
    package: Option<&str>,
    manifest_path: Option<&str>,
    extra_args: &[String],
    diagnostic_format: DiagnosticFormat,
) -> Vec<String> {
    let mut args = vec!["--color".to_string(), "never".to_string()];
    args.push(subcommand.to_string());
    if diagnostic_format == DiagnosticFormat::Json
        && subcommand_accepts_json_diagnostics(subcommand)
    {
        args.push("--message-format=json".to_string());
    }
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
    let half = max_bytes / 2;
    let head = combined
        .char_indices()
        .take_while(|(idx, _)| *idx < half)
        .last()
        .map(|(idx, ch)| &combined[..idx + ch.len_utf8()])
        .unwrap_or("");
    let tail_start = combined.len().saturating_sub(half);
    let tail_slice = &combined[tail_start..];
    let tail = if let Some(pos) = tail_slice.find('\n') {
        &tail_slice[pos + 1..]
    } else {
        tail_slice
    };
    let omitted = combined.len().saturating_sub(head.len() + tail.len());
    format!(
        "{head}\n\n[Output truncated: {omitted} bytes omitted; showing head and tail]\n\n{tail}",
    )
}

async fn read_pipe<R>(reader: R, stream_name: &'static str) -> Result<String>
where
    R: AsyncRead + Unpin,
{
    let mut reader = BufReader::new(reader).lines();
    let mut out = String::new();
    while let Some(line) = reader
        .next_line()
        .await
        .with_context(|| format!("run_cargo: read {stream_name}"))?
    {
        out.push_str(&line);
        out.push('\n');
    }
    Ok(out)
}

fn summarize_json_diagnostics(json_lines: &str) -> Option<String> {
    let mut primary = Vec::new();
    let mut notes = Vec::new();
    let mut seen_codes = BTreeSet::new();

    for line in json_lines.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if v.get("reason").and_then(Value::as_str) != Some("compiler-message") {
            continue;
        }
        let Some(message) = v.get("message") else {
            continue;
        };
        let level = message
            .get("level")
            .and_then(Value::as_str)
            .unwrap_or("diagnostic");
        if !matches!(level, "error" | "warning") {
            continue;
        }

        let code = message
            .get("code")
            .and_then(|c| c.get("code"))
            .and_then(Value::as_str);
        if let Some(code) = code {
            seen_codes.insert(code.to_string());
        }
        let text = message
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("compiler diagnostic");

        let mut location = None;
        if let Some(spans) = message.get("spans").and_then(Value::as_array) {
            for span in spans {
                if span
                    .get("is_primary")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
                {
                    let file = span.get("file_name").and_then(Value::as_str).unwrap_or("");
                    let line = span.get("line_start").and_then(Value::as_u64).unwrap_or(0);
                    let col = span
                        .get("column_start")
                        .and_then(Value::as_u64)
                        .unwrap_or(0);
                    if !file.is_empty() {
                        location = Some(format!("{file}:{line}:{col}"));
                    }
                    if let Some(label) = span.get("label").and_then(Value::as_str) {
                        if !label.is_empty() {
                            notes.push(format!("- span note: {label}"));
                        }
                    }
                    if let Some(replacement) =
                        span.get("suggested_replacement").and_then(Value::as_str)
                    {
                        notes.push(format!("- suggested replacement: `{replacement}`"));
                    }
                    break;
                }
            }
        }

        let code_text = code.map(|c| format!(" [{c}]")).unwrap_or_default();
        let loc_text = location.map(|l| format!(" at {l}")).unwrap_or_default();
        primary.push(format!("- {level}{code_text}{loc_text}: {text}"));

        if let Some(children) = message.get("children").and_then(Value::as_array) {
            for child in children.iter().take(4) {
                let child_level = child.get("level").and_then(Value::as_str).unwrap_or("note");
                let child_text = child
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if !child_text.is_empty() {
                    notes.push(format!("- {child_level}: {child_text}"));
                }
            }
        }
    }

    if primary.is_empty() && notes.is_empty() {
        return None;
    }

    primary.truncate(12);
    notes.truncate(20);

    let mut summary = String::from("Rust diagnostic summary:\n");
    if !seen_codes.is_empty() {
        summary.push_str(&format!(
            "Error/warning codes: {}\n\n",
            seen_codes.into_iter().collect::<Vec<_>>().join(", ")
        ));
    }
    if !primary.is_empty() {
        summary.push_str("Primary errors and warnings:\n");
        summary.push_str(&primary.join("\n"));
        summary.push_str("\n\n");
    }
    if !notes.is_empty() {
        summary.push_str("Helpful notes and suggestions:\n");
        summary.push_str(&notes.join("\n"));
        summary.push('\n');
    }
    Some(summary)
}

/// Run `cargo` with structured arguments; captures stdout and stderr, waits for exit.
pub async fn run_cargo(
    subcommand: &str,
    release: bool,
    package: Option<&str>,
    manifest_path: Option<&str>,
    extra_args: &[String],
    working_directory: Option<&str>,
    diagnostic_format: DiagnosticFormat,
) -> Result<String> {
    if subcommand.trim().is_empty() {
        return Err(anyhow!(
            "run_cargo: subcommand is required (e.g. check, build, test)"
        ));
    }

    let argv = cargo_executable_args_with_diagnostics(
        subcommand,
        release,
        package,
        manifest_path,
        extra_args,
        diagnostic_format,
    );
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

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("run_cargo: stdout was not piped"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| anyhow!("run_cargo: stderr was not piped"))?;

        let stdout_task = tokio::spawn(read_pipe(stdout, "stdout"));
        let stderr_task = tokio::spawn(read_pipe(stderr, "stderr"));

        let status = child
            .wait()
            .await
            .map_err(|e| anyhow!("run_cargo: wait: {}", e))?;
        let out = stdout_task
            .await
            .map_err(|e| anyhow!("run_cargo: stdout task failed: {}", e))??;
        let err = stderr_task
            .await
            .map_err(|e| anyhow!("run_cargo: stderr task failed: {}", e))??;

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
    let diagnostic_summary = if diagnostic_format == DiagnosticFormat::Json {
        summarize_json_diagnostics(&output)
    } else {
        None
    };
    if let Some(summary) = diagnostic_summary {
        combined.push_str(&summary);
        combined.push('\n');
    }
    if !output.is_empty() {
        combined.push_str("Stdout:\n");
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
    fn json_diagnostics_adds_message_format_for_check() {
        let a = cargo_executable_args_with_diagnostics(
            "check",
            false,
            None,
            None,
            &[],
            DiagnosticFormat::Json,
        );
        assert!(a.iter().any(|arg| arg == "--message-format=json"));
    }

    #[test]
    fn truncate_keeps_tail() {
        let s: String = (0..1000).map(|_| 'x').collect();
        let t = truncate_combined_output(&s, 100);
        assert!(t.contains("truncated"));
        assert!(t.len() <= s.len());
        assert!(t.ends_with('x'));
    }

    #[tokio::test]
    async fn run_cargo_json_reports_moved_value_error() {
        let root =
            std::env::temp_dir().join(format!("vybrid-run-cargo-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(
            root.join("Cargo.toml"),
            "[package]\nname = \"vybrid_cargo_test\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        std::fs::write(
            root.join("src/lib.rs"),
            "pub fn moved_value() { let s = String::from(\"x\"); let _a = s; let _b = s; }\n",
        )
        .unwrap();

        let output = run_cargo(
            "check",
            false,
            None,
            None,
            &[],
            Some(root.to_str().unwrap()),
            DiagnosticFormat::Json,
        )
        .await
        .unwrap();

        let _ = std::fs::remove_dir_all(&root);
        assert!(output.contains("Rust diagnostic summary"));
        assert!(output.contains("E0382"));
    }
}
