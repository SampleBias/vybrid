#![allow(dead_code)]

use anyhow::{anyhow, Context, Result};
use chrono::Utc;
use regex::RegexBuilder;
use serde_json::json;
use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Component, Path, PathBuf};

use crate::client::groq::Message;

const MAX_INDEX_LINE_CHARS: usize = 150;
const MAX_INDEX_CONTEXT_CHARS: usize = 4_000;
const MAX_TOPIC_CHARS: usize = 16_000;
const MAX_TRANSCRIPT_MATCHES: usize = 20;
const MAX_TRANSCRIPT_CONTEXT_LINES: usize = 2;
const MAX_TRANSCRIPT_LINE_CHARS: usize = 2_000;

#[derive(Debug, Clone)]
pub struct MemoryStore {
    project_root: PathBuf,
    memory_dir: PathBuf,
    messages_dir: PathBuf,
    session_id: String,
}

impl MemoryStore {
    pub fn new(messages_dir: impl Into<PathBuf>) -> Self {
        Self::with_project_root(
            crate::project_context::current_project_root(),
            messages_dir,
            uuid::Uuid::new_v4().to_string(),
        )
    }

    pub fn with_project_root(
        project_root: impl Into<PathBuf>,
        messages_dir: impl Into<PathBuf>,
        session_id: impl Into<String>,
    ) -> Self {
        let project_root = project_root.into();
        let memory_dir = project_root.join(".vybrid").join("memory");
        Self {
            project_root,
            memory_dir,
            messages_dir: messages_dir.into(),
            session_id: session_id.into(),
        }
    }

    pub fn core_index_path(&self) -> PathBuf {
        self.memory_dir.join("MEMORY.md")
    }

    pub fn topics_dir(&self) -> PathBuf {
        self.memory_dir.join("topics")
    }

    pub fn transcript_dir(&self) -> PathBuf {
        self.messages_dir.join(project_key(&self.project_root))
    }

    pub fn transcript_path(&self) -> PathBuf {
        self.transcript_dir()
            .join(format!("{}.jsonl", self.session_id))
    }

    pub fn read_core_index(&self) -> Result<Option<String>> {
        let path = self.core_index_path();
        if !path.exists() {
            return Ok(None);
        }

        let content = fs::read_to_string(&path)
            .with_context(|| format!("Failed to read memory index from {}", path.display()))?;
        let mut out = String::new();

        for line in content.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let capped = cap_chars(trimmed, MAX_INDEX_LINE_CHARS);
            if out.chars().count() + capped.chars().count() + 1 > MAX_INDEX_CONTEXT_CHARS {
                out.push_str("\n[Memory index truncated: narrow with topic files when needed.]");
                break;
            }
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(&capped);
        }

        if out.trim().is_empty() {
            Ok(None)
        } else {
            Ok(Some(out))
        }
    }

    pub fn context_block(&self) -> Result<Option<String>> {
        let Some(index) = self.read_core_index()? else {
            return Ok(None);
        };
        Ok(Some(format!(
            "MEMORY INDEX (skeptical, verify before acting):\n{index}\n\nUse this as pointers only. Read relevant memory topics on demand and verify remembered paths, symbols, commands, and behavior against the live project before relying on them."
        )))
    }

    pub fn list_topics(&self) -> Result<String> {
        let dir = self.topics_dir();
        if !dir.exists() {
            return Ok("No memory topics found.".to_string());
        }

        let mut topics = Vec::new();
        for entry in fs::read_dir(&dir)
            .with_context(|| format!("Failed to read memory topics from {}", dir.display()))?
        {
            let entry = entry?;
            let path = entry.path();
            if path.is_file() && path.extension().and_then(|s| s.to_str()) == Some("md") {
                if let Ok(relative) = path.strip_prefix(&dir) {
                    topics.push(
                        relative
                            .with_extension("")
                            .to_string_lossy()
                            .replace('\\', "/"),
                    );
                }
            }
        }
        topics.sort();

        if topics.is_empty() {
            Ok("No memory topics found.".to_string())
        } else {
            Ok(format!("Memory topics:\n{}", topics.join("\n")))
        }
    }

    pub fn read_topic(&self, topic: &str) -> Result<String> {
        let path = self.topic_path(topic)?;
        if !path.exists() {
            return Ok(format!(
                "Memory topic not found: `{}` (expected {}).",
                topic.trim(),
                path.display()
            ));
        }
        let content = fs::read_to_string(&path)
            .with_context(|| format!("Failed to read memory topic {}", path.display()))?;
        let capped = cap_chars(&content, MAX_TOPIC_CHARS);
        let mut out = format!("Memory topic `{}`:\n\n{}", topic.trim(), capped);
        if capped.chars().count() < content.chars().count() {
            out.push_str("\n\n[Memory topic truncated. Use a narrower topic or verify details in the live codebase.]");
        }
        Ok(out)
    }

    pub fn append_transcript_message(&self, message: &Message) -> Result<()> {
        let path = self.transcript_path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create transcript dir {}", parent.display()))?;
        }

        let line = serde_json::to_string(&json!({
            "timestamp": Utc::now().to_rfc3339(),
            "project_root": self.project_root.display().to_string(),
            "role": message.role,
            "content": message.content,
            "tool_calls": message.tool_calls,
            "tool_call_id": message.tool_call_id,
        }))?;

        use std::io::Write;
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .with_context(|| format!("Failed to open transcript {}", path.display()))?;
        writeln!(file, "{line}")
            .with_context(|| format!("Failed to append transcript {}", path.display()))?;
        Ok(())
    }

    pub fn search_transcripts(
        &self,
        query: &str,
        case_sensitive: bool,
        max_matches: usize,
    ) -> Result<String> {
        let query = query.trim();
        if query.len() < 2 {
            return Err(anyhow!(
                "search_memory_transcripts requires a specific identifier, path, symbol, or error code of at least 2 characters"
            ));
        }

        let dir = self.transcript_dir();
        if !dir.exists() {
            return Ok(format!("No raw transcripts found for `{query}`."));
        }

        let regex = RegexBuilder::new(&regex::escape(query))
            .case_insensitive(!case_sensitive)
            .build()
            .with_context(|| format!("Failed to build transcript search regex for `{query}`"))?;
        let max_matches = max_matches.clamp(1, MAX_TRANSCRIPT_MATCHES);
        let mut results = Vec::new();
        let mut count = 0usize;

        let mut paths = transcript_files(&dir)?;
        paths.sort();
        for path in paths {
            if count >= max_matches {
                break;
            }
            let content = fs::read_to_string(&path)
                .with_context(|| format!("Failed to read transcript {}", path.display()))?;
            let lines: Vec<&str> = content.lines().collect();
            for (line_idx, line) in lines.iter().enumerate() {
                if count >= max_matches {
                    break;
                }
                if !regex.is_match(line) {
                    continue;
                }

                let start = line_idx.saturating_sub(MAX_TRANSCRIPT_CONTEXT_LINES);
                let end = (line_idx + MAX_TRANSCRIPT_CONTEXT_LINES + 1).min(lines.len());
                let mut block = format!(
                    "=== {}:{} ===\n",
                    path.file_name()
                        .and_then(|s| s.to_str())
                        .unwrap_or("transcript"),
                    line_idx + 1
                );
                for (idx, ctx_line) in lines.iter().enumerate().take(end).skip(start) {
                    let prefix = if idx == line_idx { ">" } else { " " };
                    block.push_str(&format!(
                        "{} {:4}: {}\n",
                        prefix,
                        idx + 1,
                        cap_chars(ctx_line, MAX_TRANSCRIPT_LINE_CHARS)
                    ));
                }
                results.push(block);
                count += 1;
            }
        }

        if results.is_empty() {
            Ok(format!("No raw transcript matches found for `{query}`."))
        } else {
            Ok(format!(
                "Found {count} transcript match(es) for `{query}`. Treat these as hints and verify against the live codebase before acting.\n\n{}",
                results.join("\n")
            ))
        }
    }

    fn topic_path(&self, topic: &str) -> Result<PathBuf> {
        let topic = topic.trim().trim_end_matches(".md");
        if topic.is_empty() {
            return Err(anyhow!("read_memory_topic requires a topic name"));
        }

        let relative = Path::new(topic);
        if relative.is_absolute() {
            return Err(anyhow!(
                "memory topic must be relative to the topics directory"
            ));
        }
        for component in relative.components() {
            match component {
                Component::Normal(_) => {}
                _ => return Err(anyhow!("memory topic cannot contain `..` or path prefixes")),
            }
        }

        Ok(self.topics_dir().join(relative).with_extension("md"))
    }
}

fn cap_chars(input: &str, max_chars: usize) -> String {
    if input.chars().count() <= max_chars {
        return input.to_string();
    }
    let keep = max_chars.saturating_sub(1);
    format!("{}…", input.chars().take(keep).collect::<String>())
}

fn project_key(root: &Path) -> String {
    let mut hasher = DefaultHasher::new();
    root.display().to_string().hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

fn transcript_files(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    for entry in fs::read_dir(dir)
        .with_context(|| format!("Failed to read transcript dir {}", dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        if path.is_file() && path.extension().and_then(|s| s.to_str()) == Some("jsonl") {
            paths.push(path);
        }
    }
    Ok(paths)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "vybrid-memory-{name}-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn core_index_caps_lines_and_total_context() {
        let root = temp_root("index");
        let messages = root.join("messages");
        let store = MemoryStore::with_project_root(&root, &messages, "session");
        fs::create_dir_all(store.core_index_path().parent().unwrap()).unwrap();
        fs::write(
            store.core_index_path(),
            format!("{}\nshort pointer\n", "x".repeat(300)),
        )
        .unwrap();

        let index = store.read_core_index().unwrap().unwrap();
        let first = index.lines().next().unwrap();
        assert!(first.chars().count() <= MAX_INDEX_LINE_CHARS);
        assert!(index.contains("short pointer"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn topic_paths_reject_parent_segments() {
        let root = temp_root("topic-path");
        let store = MemoryStore::with_project_root(&root, root.join("messages"), "session");

        assert!(store.topic_path("../secret").is_err());
        assert!(store.topic_path("/tmp/secret").is_err());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn reads_topic_with_cap() {
        let root = temp_root("topic");
        let store = MemoryStore::with_project_root(&root, root.join("messages"), "session");
        fs::create_dir_all(store.topics_dir()).unwrap();
        fs::write(store.topics_dir().join("routing.md"), "topic body").unwrap();

        let topic = store.read_topic("routing").unwrap();
        assert!(topic.contains("topic body"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn transcript_search_is_specific_and_bounded() {
        let root = temp_root("transcript");
        let store = MemoryStore::with_project_root(&root, root.join("messages"), "session");
        let msg = Message {
            role: "assistant".to_string(),
            content: Some("Use MemoryStore when wiring skeptical memory.".to_string()),
            tool_calls: None,
            tool_call_id: None,
        };
        store.append_transcript_message(&msg).unwrap();

        assert!(store.search_transcripts("M", false, 10).is_err());
        let result = store.search_transcripts("MemoryStore", true, 10).unwrap();
        assert!(result.contains("MemoryStore"));
        assert!(result.contains("verify against the live codebase"));

        let _ = fs::remove_dir_all(root);
    }
}
