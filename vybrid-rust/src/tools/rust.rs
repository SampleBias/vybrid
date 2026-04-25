//! Rust-specific helper tools for compiler diagnostics and project discovery.

use anyhow::{anyhow, Result};
use serde_json::Value;
use std::path::Path;
use std::time::Duration;
use tokio::process::Command;
use tokio::time::timeout;

use super::file_ops::normalize_path;

const MAX_RUST_HELP_BYTES: usize = 64 * 1024;
const MAX_METADATA_BYTES: usize = 256 * 1024;

fn truncate_text(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_string();
    }
    let half = max_bytes / 2;
    let head = &text[..half.min(text.len())];
    let tail_start = text.len().saturating_sub(half);
    let tail = &text[tail_start..];
    format!(
        "{head}\n\n[Output truncated: {} bytes omitted]\n\n{tail}",
        text.len().saturating_sub(head.len() + tail.len())
    )
}

fn concept_hint(code_or_topic: &str) -> Option<&'static str> {
    match code_or_topic.trim().to_ascii_lowercase().as_str() {
        "e0382" | "moved value" | "move" => Some(
            "Moved value: decide whether the value should be owned, borrowed, cloned, or restructured so ownership moves only once.",
        ),
        "e0499" | "mutable borrow" => Some(
            "Overlapping mutable borrows: Rust allows either one mutable reference or many shared references. Shorten borrow scopes or split data.",
        ),
        "e0502" | "borrow conflict" => Some(
            "Shared vs mutable borrow conflict: make the immutable borrow end before the mutable borrow, or restructure the data flow.",
        ),
        "lifetime" | "lifetimes" => Some(
            "Lifetimes describe relationships between references. Add named lifetimes only when output references must be tied to inputs.",
        ),
        "trait" | "traits" | "trait bound" => Some(
            "Trait bound failures usually mean the generic contract is missing, implemented for the wrong type, or needs an associated type/lifetime bound.",
        ),
        "enum" | "enums" | "match" => Some(
            "Enums model alternatives. Prefer exhaustive `match`, use `_` sparingly in public logic, and let pattern matching expose invalid states.",
        ),
        "send" | "async send" => Some(
            "Async `Send` errors often come from holding non-Send values like `Rc` or `RefCell` across `.await`; use `Arc`, async-aware locks, or shorten scopes.",
        ),
        _ => None,
    }
}

/// Explain a rustc diagnostic code or common Rust topic.
pub async fn explain_rust_diagnostic(code_or_topic: &str) -> Result<String> {
    let query = code_or_topic.trim();
    if query.is_empty() {
        return Err(anyhow!(
            "explain_rust_diagnostic: provide an error code like E0382 or a Rust topic"
        ));
    }

    let mut result = String::new();
    if let Some(hint) = concept_hint(query) {
        result.push_str("Rust concept hint:\n");
        result.push_str(hint);
        result.push_str("\n\n");
    }

    let code = query.trim_start_matches("error[").trim_end_matches(']');
    if code.len() == 5 && code.starts_with('E') && code[1..].chars().all(|c| c.is_ascii_digit()) {
        let run = Command::new("rustc").arg("--explain").arg(code).output();
        match timeout(Duration::from_secs(10), run).await {
            Ok(Ok(output)) if output.status.success() => {
                result.push_str(&format!("rustc --explain {code}:\n"));
                result.push_str(&String::from_utf8_lossy(&output.stdout));
            }
            Ok(Ok(output)) => {
                result.push_str(&format!(
                    "rustc --explain {code} failed with exit code {:?}:\n{}",
                    output.status.code(),
                    String::from_utf8_lossy(&output.stderr)
                ));
            }
            Ok(Err(e)) => result.push_str(&format!("Could not run rustc --explain: {e}")),
            Err(_) => result.push_str("rustc --explain timed out."),
        }
    }

    if result.trim().is_empty() {
        result.push_str("No built-in explanation found. Try an error code like E0382, E0499, E0502, or a topic like traits, enums, lifetimes, or async Send.");
    }

    Ok(truncate_text(&result, MAX_RUST_HELP_BYTES))
}

/// Run `cargo metadata --format-version=1 --no-deps` and return JSON.
pub async fn cargo_metadata(
    manifest_path: Option<&str>,
    working_directory: Option<&str>,
) -> Result<String> {
    let mut cmd = Command::new("cargo");
    cmd.args([
        "metadata",
        "--format-version=1",
        "--no-deps",
        "--color",
        "never",
    ]);
    cmd.env("CARGO_TERM_COLOR", "never");
    cmd.kill_on_drop(true);

    if let Some(manifest) = manifest_path.filter(|m| !m.trim().is_empty()) {
        cmd.arg("--manifest-path").arg(normalize_path(manifest));
    }
    if let Some(dir) = working_directory.filter(|d| !d.trim().is_empty()) {
        let normalized = normalize_path(dir);
        if !Path::new(&normalized).exists() {
            return Err(anyhow!(
                "cargo_metadata: working_directory does not exist: {normalized}"
            ));
        }
        cmd.current_dir(normalized);
    }

    let output = timeout(Duration::from_secs(60), cmd.output())
        .await
        .map_err(|_| anyhow!("cargo_metadata: timed out after 60s"))?
        .map_err(|e| anyhow!("cargo_metadata: failed to spawn cargo: {e}"))?;

    if !output.status.success() {
        return Err(anyhow!(
            "cargo_metadata failed (exit code {:?}):\n{}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    Ok(truncate_text(
        &String::from_utf8_lossy(&output.stdout),
        MAX_METADATA_BYTES,
    ))
}

/// Summarize the current Rust package/workspace from Cargo metadata.
pub async fn rust_project_snapshot(
    manifest_path: Option<&str>,
    working_directory: Option<&str>,
) -> Result<String> {
    let metadata = cargo_metadata(manifest_path, working_directory).await?;
    let v: Value = serde_json::from_str(&metadata)
        .map_err(|e| anyhow!("rust_project_snapshot: failed to parse cargo metadata: {e}"))?;

    let workspace_members = v
        .get("workspace_members")
        .and_then(Value::as_array)
        .map(|members| members.len())
        .unwrap_or(0);

    let mut out = String::new();
    out.push_str("Rust project snapshot:\n");
    out.push_str(&format!("- workspace members: {workspace_members}\n"));

    if let Some(packages) = v.get("packages").and_then(Value::as_array) {
        out.push_str("\nPackages:\n");
        for package in packages.iter().take(20) {
            let name = package
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("<unnamed>");
            let version = package.get("version").and_then(Value::as_str).unwrap_or("");
            let edition = package
                .get("edition")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            out.push_str(&format!("- {name} {version} (edition {edition})\n"));

            if let Some(targets) = package.get("targets").and_then(Value::as_array) {
                let target_list = targets
                    .iter()
                    .filter_map(|target| {
                        let name = target.get("name").and_then(Value::as_str)?;
                        let kinds = target
                            .get("kind")
                            .and_then(Value::as_array)
                            .map(|k| {
                                k.iter()
                                    .filter_map(Value::as_str)
                                    .collect::<Vec<_>>()
                                    .join("/")
                            })
                            .unwrap_or_default();
                        Some(format!("{name} ({kinds})"))
                    })
                    .collect::<Vec<_>>();
                if !target_list.is_empty() {
                    out.push_str(&format!("  targets: {}\n", target_list.join(", ")));
                }
            }

            if let Some(features) = package.get("features").and_then(Value::as_object) {
                let names = features.keys().take(12).cloned().collect::<Vec<_>>();
                if !names.is_empty() {
                    out.push_str(&format!("  features: {}\n", names.join(", ")));
                }
            }

            if let Some(deps) = package.get("dependencies").and_then(Value::as_array) {
                let dep_names = deps
                    .iter()
                    .filter_map(|dep| dep.get("name").and_then(Value::as_str))
                    .take(20)
                    .collect::<Vec<_>>();
                if !dep_names.is_empty() {
                    out.push_str(&format!("  dependencies: {}\n", dep_names.join(", ")));
                }
            }
        }
    }

    Ok(truncate_text(&out, MAX_RUST_HELP_BYTES))
}
