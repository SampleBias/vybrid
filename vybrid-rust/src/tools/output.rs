#![allow(dead_code)]

use anyhow::{Context, Result};
use chrono::Utc;
use std::collections::VecDeque;
use std::fs;
use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};

const OFFLOAD_THRESHOLD_BYTES: usize = 24 * 1024;
const PREVIEW_BYTES: usize = 8 * 1024;

/// Keep byte-bounded head/tail excerpts without splitting UTF-8 code points.
pub fn truncate_utf8_middle(input: &str, max_bytes: usize, label: &str) -> String {
    if input.len() <= max_bytes {
        return input.to_owned();
    }
    let mut head = max_bytes / 2;
    while !input.is_char_boundary(head) {
        head -= 1;
    }
    let mut tail = input.len() - max_bytes / 2;
    while !input.is_char_boundary(tail) {
        tail += 1;
    }
    format!(
        "{}\n\n[{label} truncated: {} bytes omitted]\n\n{}",
        &input[..head],
        tail - head,
        &input[tail..]
    )
}

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

    fn new_path(&self, name: &str) -> PathBuf {
        self.dir.join(format!(
            "{}-{}.txt",
            sanitize_tool_name(name),
            uuid::Uuid::new_v4()
        ))
    }

    pub fn maybe_offload(&self, tool_name: &str, output: String) -> Result<String> {
        if output.len() <= OFFLOAD_THRESHOLD_BYTES {
            return Ok(output);
        }

        fs::create_dir_all(&self.dir)
            .with_context(|| format!("Failed to create tool output dir {}", self.dir.display()))?;
        let id = self.counter.fetch_add(1, Ordering::Relaxed) + 1;
        let file_name = format!(
            "{}-{}-{id}-{}.txt",
            Utc::now().format("%Y%m%dT%H%M%SZ"),
            sanitize_tool_name(tool_name),
            uuid::Uuid::new_v4()
        );
        let path = self.dir.join(file_name);
        fs::write(&path, &output)
            .with_context(|| format!("Failed to offload tool result {}", path.display()))?;

        let preview = truncate_utf8_middle(&output, PREVIEW_BYTES, "Preview");
        let truncated = output.len() > PREVIEW_BYTES;
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

/// Raw bytes are streamed to disk after the inline budget is reached. The
/// in-memory preview and compiler-line collector stay bounded even for one
/// enormous line. JSON mode archives even small outputs for exact retrieval.
pub struct CapturedOutput {
    pub text: String,
    pub diagnostic_lines: String,
    pub plain_text: String,
    pub artifact: Option<PathBuf>,
    pub bytes: u64,
    pub diagnostics_omitted: bool,
}

#[derive(Default)]
struct CompilerLines {
    pending: Vec<u8>,
    discarding: bool,
    diagnostics: VecDeque<String>,
    diagnostic_bytes: usize,
    plain: String,
    omitted: bool,
}

impl CompilerLines {
    fn push(&mut self, bytes: &[u8]) {
        for part in bytes.split_inclusive(|b| *b == b'\n') {
            if !self.discarding {
                if self.pending.len() + part.len() <= 256 * 1024 {
                    self.pending.extend_from_slice(part);
                } else {
                    self.pending.clear();
                    self.discarding = true;
                    self.omitted = true;
                }
            }
            if part.ends_with(b"\n") {
                if !self.discarding {
                    self.finish_line();
                }
                self.discarding = false;
            }
        }
    }

    fn finish_line(&mut self) {
        if self.pending.is_empty() {
            return;
        }
        let line = String::from_utf8_lossy(&self.pending).into_owned();
        self.pending.clear();
        match serde_json::from_str::<serde_json::Value>(&line) {
            Ok(value) if value["reason"] == "compiler-message" => {
                while self.diagnostic_bytes + line.len() > 256 * 1024 {
                    let Some(old) = self.diagnostics.pop_front() else {
                        break;
                    };
                    self.diagnostic_bytes -= old.len();
                    self.omitted = true;
                }
                self.diagnostic_bytes += line.len();
                self.diagnostics.push_back(line);
            }
            Ok(value) if value.get("reason").is_some() => {}
            _ => {
                self.plain.push_str(&line);
                self.plain = truncate_utf8_middle(&self.plain, PREVIEW_BYTES, "Program output");
            }
        }
    }
}

pub async fn capture_pipe<R: AsyncRead + Unpin>(
    mut reader: R,
    store: ToolOutputStore,
    name: &str,
    compiler_json: bool,
) -> Result<CapturedOutput> {
    let mut inline = Vec::new();
    let mut head = Vec::new();
    let mut tail = Vec::new();
    let mut file = None;
    let mut artifact = None;
    let mut total = 0u64;
    let mut compiler = CompilerLines::default();
    let mut chunk = [0u8; 8192];
    loop {
        let count = reader.read(&mut chunk).await?;
        if count == 0 {
            break;
        }
        let bytes = &chunk[..count];
        total += count as u64;
        if compiler_json {
            compiler.push(bytes);
        }
        let head_room = (PREVIEW_BYTES / 2usize).saturating_sub(head.len());
        head.extend_from_slice(&bytes[..bytes.len().min(head_room)]);
        tail.extend_from_slice(bytes);
        if tail.len() > PREVIEW_BYTES / 2 {
            tail.drain(..tail.len() - PREVIEW_BYTES / 2);
        }
        if file.is_none() && (compiler_json || inline.len() + count > OFFLOAD_THRESHOLD_BYTES) {
            tokio::fs::create_dir_all(&store.dir).await?;
            let path = store.new_path(name);
            let mut output = tokio::fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&path)
                .await?;
            output.write_all(&inline).await?;
            inline.clear();
            artifact = Some(path);
            file = Some(output);
        }
        if let Some(output) = file.as_mut() {
            output.write_all(bytes).await?;
        } else {
            inline.extend_from_slice(bytes);
        }
    }
    if let Some(output) = file.as_mut() {
        output.flush().await?;
    }
    compiler.finish_line();
    let text = if let Some(path) = &artifact {
        if total <= PREVIEW_BYTES as u64 {
            // The head and tail overlap for small archived outputs. Join them
            // before decoding so a character split at either edge stays intact.
            let overlap = head.len() + tail.len() - total as usize;
            head.extend_from_slice(&tail[overlap..]);
            format!(
                "Full {name}: `{}` ({} bytes)\n{}",
                path.display(),
                total,
                String::from_utf8_lossy(&head)
            )
        } else {
            // Align the two preview edges; raw artifacts preserve every original byte.
            while std::str::from_utf8(&head).is_err() && !head.is_empty() {
                head.pop();
            }
            let mut start = 0;
            while start < tail.len() && (tail[start] & 0xc0) == 0x80 {
                start += 1;
            }
            format!(
                "Full {name}: `{}` ({} bytes; use read_file ranges)\n{}\n[Middle omitted]\n{}",
                path.display(),
                total,
                String::from_utf8_lossy(&head),
                String::from_utf8_lossy(&tail[start..])
            )
        }
    } else {
        String::from_utf8_lossy(&inline).into_owned()
    };
    Ok(CapturedOutput {
        text,
        diagnostic_lines: compiler.diagnostics.into_iter().collect(),
        plain_text: compiler.plain,
        artifact,
        bytes: total,
        diagnostics_omitted: compiler.omitted,
    })
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
    fn previews_preserve_unicode_for_every_small_budget() {
        let text = "é€🦀".repeat(100);
        for budget in 0..100 {
            let preview = truncate_utf8_middle(&text, budget, "test");
            assert!(!preview.contains('�'));
            assert!(preview.contains("truncated"));
        }
    }

    #[test]
    fn compiler_collection_bounds_a_single_huge_line() {
        let mut collector = CompilerLines::default();
        for _ in 0..100 {
            collector.push(&[b'x'; 8192]);
        }
        assert!(collector.pending.len() <= 256 * 1024);
        assert!(collector.omitted);
        collector.push(b"\nplain test output\n");
        assert!(collector.plain.contains("plain test output"));
    }

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
