#![allow(dead_code)]

use anyhow::{Context, Result};
use chrono::Utc;
use std::fs;
use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};

const OFFLOAD_THRESHOLD_BYTES: usize = 24 * 1024;
const PREVIEW_BYTES: usize = 8 * 1024;

#[derive(Debug, Clone)]
pub struct ToolOutputStore {
    dir: PathBuf,
    counter: Arc<AtomicU64>,
}

impl ToolOutputStore {
    pub fn new(base_dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: base_dir.into().join("tool-results"),
            counter: Arc::new(AtomicU64::new(0)),
        }
    }

    pub fn maybe_offload(&self, tool_name: &str, output: String) -> Result<String> {
        if output.len() <= OFFLOAD_THRESHOLD_BYTES {
            return Ok(output);
        }

        fs::create_dir_all(&self.dir)
            .with_context(|| format!("Failed to create tool output dir {}", self.dir.display()))?;
        let id = self.counter.fetch_add(1, Ordering::Relaxed) + 1;
        let file_name = format!(
            "{}-{}-{id}.txt",
            Utc::now().format("%Y%m%dT%H%M%SZ"),
            sanitize_tool_name(tool_name)
        );
        let path = self.dir.join(file_name);
        fs::write(&path, &output)
            .with_context(|| format!("Failed to offload tool result {}", path.display()))?;

        let (preview, truncated) = preview_text(&output, PREVIEW_BYTES);
        let omitted_note = if truncated {
            format!(
                "\n\n[Preview truncated: full result is {} bytes.]",
                output.len()
            )
        } else {
            String::new()
        };
        Ok(format!(
            "[Vybrid offloaded large `{tool_name}` result]\nFull result: `{}`\nRead it with `read_file` if exact omitted content is needed. Use this preview for orientation only.\n\n{preview}{omitted_note}",
            path.display()
        ))
    }
}

impl Default for ToolOutputStore {
    fn default() -> Self {
        let base = dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".vybrid")
            .join("progress");
        Self::new(base)
    }
}

fn sanitize_tool_name(tool_name: &str) -> String {
    let sanitized: String = tool_name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    if sanitized.is_empty() {
        "tool".to_string()
    } else {
        sanitized
    }
}

fn preview_text(input: &str, max_bytes: usize) -> (String, bool) {
    if input.len() <= max_bytes {
        return (input.to_string(), false);
    }
    let mut end = max_bytes.min(input.len());
    while end > 0 && !input.is_char_boundary(end) {
        end -= 1;
    }
    (input[..end].to_string(), true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offloads_large_output_and_returns_preview_reference() {
        let root = std::env::temp_dir().join(format!(
            "vybrid-output-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let store = ToolOutputStore::new(&root);
        let output = "needle\n".repeat(5_000);

        let returned = store
            .maybe_offload("enhanced_grep", output.clone())
            .unwrap();

        assert!(returned.contains("offloaded large `enhanced_grep` result"));
        assert!(returned.contains("Full result:"));
        assert!(returned.len() < output.len());
        let files = fs::read_dir(root.join("tool-results"))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(fs::read_to_string(files[0].path()).unwrap(), output);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn keeps_small_output_inline() {
        let root = std::env::temp_dir().join(format!(
            "vybrid-output-small-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let store = ToolOutputStore::new(&root);

        let returned = store
            .maybe_offload("read_file", "small".to_string())
            .unwrap();

        assert_eq!(returned, "small");
        assert!(!root.join("tool-results").exists());

        let _ = fs::remove_dir_all(root);
    }
}
