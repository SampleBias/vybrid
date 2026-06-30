#![allow(dead_code)]

use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Duration, Utc};
use regex::RegexBuilder;
use serde::{Deserialize, Serialize};
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
const AUTODREAM_MIN_SESSIONS: usize = 5;
const AUTODREAM_MIN_INTERVAL_HOURS: i64 = 24;
const AUTODREAM_MAX_LINES: usize = 200;
const AUTODREAM_MAX_BYTES: usize = 25 * 1024;
const AUTODREAM_TOPIC: &str = "autodream";
const AUTODREAM_TOPIC_FILE: &str = "autodream.md";
const AUTODREAM_INDEX_POINTER: &str =
    "- autoDream: topics/autodream.md — consolidated recent sessions; verify details before use.";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AutoDreamOutcome {
    Consolidated {
        sessions: usize,
        topics: usize,
        transcript_matches: usize,
    },
    Skipped(AutoDreamSkipReason),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutoDreamSkipReason {
    RanRecently,
    NotEnoughSessions,
    Locked,
    NoNewTranscriptData,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct AutoDreamState {
    last_run: Option<String>,
    sessions_since_last_run: Vec<String>,
}

struct AutoDreamLock {
    path: PathBuf,
}

impl Drop for AutoDreamLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

#[derive(Debug)]
struct Orientation {
    topic_count: usize,
    total_lines: usize,
    total_bytes: usize,
}

#[derive(Debug, Default)]
struct GatheredMemory {
    user_requests: Vec<String>,
    tool_names: Vec<String>,
    paths: Vec<String>,
    transcript_matches: usize,
}

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

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// Rebind memory to the current project root after the user changes directory.
    /// Returns true when the project root changed (starts a new transcript session id).
    pub fn sync_project_root(&mut self) -> bool {
        let root = crate::project_context::current_project_root();
        if root == self.project_root {
            return false;
        }
        self.project_root = root.clone();
        self.memory_dir = root.join(".vybrid").join("memory");
        self.session_id = uuid::Uuid::new_v4().to_string();
        true
    }

    pub fn auto_dream_state_path(&self) -> PathBuf {
        self.memory_dir.join("autodream_state.json")
    }

    pub fn auto_dream_lock_path(&self) -> PathBuf {
        self.memory_dir.join("autodream.lock")
    }

    pub fn complete_session_and_autodream(&self) -> Result<AutoDreamOutcome> {
        self.record_session_completed()?;
        self.run_autodream_if_due()
    }

    fn record_session_completed(&self) -> Result<()> {
        let mut state = self.load_autodream_state()?;
        if !state
            .sessions_since_last_run
            .iter()
            .any(|id| id == &self.session_id)
        {
            state.sessions_since_last_run.push(self.session_id.clone());
            self.save_autodream_state(&state)?;
        }
        Ok(())
    }

    pub fn run_autodream_if_due(&self) -> Result<AutoDreamOutcome> {
        let mut state = self.load_autodream_state()?;
        if !autodream_interval_elapsed(&state) {
            return Ok(AutoDreamOutcome::Skipped(AutoDreamSkipReason::RanRecently));
        }
        if state.sessions_since_last_run.len() < AUTODREAM_MIN_SESSIONS {
            return Ok(AutoDreamOutcome::Skipped(
                AutoDreamSkipReason::NotEnoughSessions,
            ));
        }

        let Some(_lock) = self.try_acquire_autodream_lock()? else {
            return Ok(AutoDreamOutcome::Skipped(AutoDreamSkipReason::Locked));
        };

        let orientation = self.orient_memory()?;
        let gathered = self.gather_transcript_memory(&state.sessions_since_last_run)?;
        if gathered.transcript_matches == 0 {
            state.sessions_since_last_run.clear();
            state.last_run = Some(Utc::now().to_rfc3339());
            self.save_autodream_state(&state)?;
            return Ok(AutoDreamOutcome::Skipped(
                AutoDreamSkipReason::NoNewTranscriptData,
            ));
        }

        self.consolidate_memory_topic(&orientation, &gathered)?;
        self.prune_memory_budget()?;

        let sessions = state.sessions_since_last_run.len();
        state.sessions_since_last_run.clear();
        state.last_run = Some(Utc::now().to_rfc3339());
        self.save_autodream_state(&state)?;

        Ok(AutoDreamOutcome::Consolidated {
            sessions,
            topics: orientation.topic_count,
            transcript_matches: gathered.transcript_matches,
        })
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

    fn load_autodream_state(&self) -> Result<AutoDreamState> {
        let path = self.auto_dream_state_path();
        if !path.exists() {
            return Ok(AutoDreamState::default());
        }
        let content = fs::read_to_string(&path)
            .with_context(|| format!("Failed to read autoDream state {}", path.display()))?;
        serde_json::from_str(&content)
            .with_context(|| format!("Failed to parse autoDream state {}", path.display()))
    }

    fn save_autodream_state(&self, state: &AutoDreamState) -> Result<()> {
        let path = self.auto_dream_state_path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create memory dir {}", parent.display()))?;
        }
        let content = serde_json::to_string_pretty(state)?;
        fs::write(&path, content + "\n")
            .with_context(|| format!("Failed to write autoDream state {}", path.display()))?;
        Ok(())
    }

    fn try_acquire_autodream_lock(&self) -> Result<Option<AutoDreamLock>> {
        let path = self.auto_dream_lock_path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create memory dir {}", parent.display()))?;
        }

        let lock = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path);
        match lock {
            Ok(mut file) => {
                use std::io::Write;
                writeln!(file, "pid={}", std::process::id()).with_context(|| {
                    format!("Failed to write autoDream lock {}", path.display())
                })?;
                Ok(Some(AutoDreamLock { path }))
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(None),
            Err(e) => Err(e)
                .with_context(|| format!("Failed to create autoDream lock {}", path.display())),
        }
    }

    fn orient_memory(&self) -> Result<Orientation> {
        let mut topic_count = 0usize;
        let mut total_lines = 0usize;
        let mut total_bytes = 0usize;

        for path in self.memory_files()? {
            if path
                .strip_prefix(self.topics_dir())
                .map(|_| true)
                .unwrap_or(false)
            {
                topic_count += 1;
            }
            let content = fs::read_to_string(&path)
                .with_context(|| format!("Failed to inspect memory file {}", path.display()))?;
            total_lines += content.lines().count();
            total_bytes += content.len();
        }

        Ok(Orientation {
            topic_count,
            total_lines,
            total_bytes,
        })
    }

    fn gather_transcript_memory(&self, session_ids: &[String]) -> Result<GatheredMemory> {
        let mut gathered = GatheredMemory::default();
        for session_id in session_ids {
            let path = self.transcript_dir().join(format!("{session_id}.jsonl"));
            if !path.exists() {
                continue;
            }
            let content = fs::read_to_string(&path)
                .with_context(|| format!("Failed to gather transcript {}", path.display()))?;
            for line in content.lines() {
                let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
                    continue;
                };
                let role = value["role"].as_str().unwrap_or("");
                let content = value["content"].as_str().unwrap_or("").trim();
                if role == "user" && !content.is_empty() {
                    push_unique_capped(
                        &mut gathered.user_requests,
                        first_user_request_line(content),
                        120,
                        20,
                    );
                    for path in extract_path_mentions(content) {
                        push_unique_capped(&mut gathered.paths, &path, 120, 30);
                    }
                    gathered.transcript_matches += 1;
                }
                if let Some(tool_calls) = value["tool_calls"].as_array() {
                    for tool_call in tool_calls {
                        if let Some(name) = tool_call["function"]["name"].as_str() {
                            push_unique_capped(&mut gathered.tool_names, name, 80, 20);
                            gathered.transcript_matches += 1;
                        }
                    }
                }
            }
        }
        Ok(gathered)
    }

    fn consolidate_memory_topic(
        &self,
        orientation: &Orientation,
        gathered: &GatheredMemory,
    ) -> Result<()> {
        fs::create_dir_all(self.topics_dir()).with_context(|| {
            format!(
                "Failed to create topics dir {}",
                self.topics_dir().display()
            )
        })?;

        let mut topic = String::new();
        topic.push_str("# autoDream Consolidated Memory\n\n");
        topic.push_str(&format!(
            "Generated: {}\n\n",
            Utc::now().format("%Y-%m-%dT%H:%M:%SZ")
        ));
        topic.push_str("Memory is skeptical: verify these observations against live files, tools, and compiler output before acting.\n\n");
        topic.push_str(&format!(
            "Orient: saw {} topic file(s), {} memory line(s), {} memory byte(s) before consolidation.\n\n",
            orientation.topic_count, orientation.total_lines, orientation.total_bytes
        ));
        topic.push_str("## Recent User Requests\n");
        append_bullets(&mut topic, &gathered.user_requests);
        topic.push_str("\n## Tool Activity\n");
        append_bullets(&mut topic, &gathered.tool_names);
        topic.push_str("\n## Mentioned Paths\n");
        append_bullets(&mut topic, &gathered.paths);

        fs::write(self.topics_dir().join(AUTODREAM_TOPIC_FILE), topic)
            .with_context(|| "Failed to write autoDream memory topic")?;
        self.upsert_autodream_index_pointer()?;
        Ok(())
    }

    fn upsert_autodream_index_pointer(&self) -> Result<()> {
        let path = self.core_index_path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create memory dir {}", parent.display()))?;
        }

        let existing = if path.exists() {
            fs::read_to_string(&path)
                .with_context(|| format!("Failed to read memory index {}", path.display()))?
        } else {
            String::new()
        };
        let mut lines: Vec<String> = existing
            .lines()
            .filter(|line| !line.contains("topics/autodream.md"))
            .map(str::to_string)
            .collect();
        lines.push(AUTODREAM_INDEX_POINTER.to_string());
        fs::write(&path, lines.join("\n") + "\n")
            .with_context(|| format!("Failed to write memory index {}", path.display()))?;
        Ok(())
    }

    fn prune_memory_budget(&self) -> Result<()> {
        prune_file_lines_and_bytes(
            &self.core_index_path(),
            AUTODREAM_MAX_LINES,
            AUTODREAM_MAX_BYTES,
        )?;

        let autodream_path = self.topics_dir().join(AUTODREAM_TOPIC_FILE);
        let other = self
            .memory_files()?
            .into_iter()
            .filter(|path| path != &autodream_path)
            .try_fold((0usize, 0usize), |(lines, bytes), path| -> Result<_> {
                let content = fs::read_to_string(&path)
                    .with_context(|| format!("Failed to inspect memory file {}", path.display()))?;
                Ok((lines + content.lines().count(), bytes + content.len()))
            })?;

        let remaining_lines = AUTODREAM_MAX_LINES.saturating_sub(other.0).max(1);
        let remaining_bytes = AUTODREAM_MAX_BYTES.saturating_sub(other.1).max(256);
        prune_file_lines_and_bytes(&autodream_path, remaining_lines, remaining_bytes)?;
        Ok(())
    }

    fn memory_files(&self) -> Result<Vec<PathBuf>> {
        let mut files = Vec::new();
        let index = self.core_index_path();
        if index.exists() {
            files.push(index);
        }
        let topics_dir = self.topics_dir();
        if topics_dir.exists() {
            for entry in fs::read_dir(&topics_dir)
                .with_context(|| format!("Failed to read topics dir {}", topics_dir.display()))?
            {
                let path = entry?.path();
                if path.is_file() && path.extension().and_then(|s| s.to_str()) == Some("md") {
                    files.push(path);
                }
            }
        }
        Ok(files)
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

fn autodream_interval_elapsed(state: &AutoDreamState) -> bool {
    let Some(last_run) = state.last_run.as_deref() else {
        return true;
    };
    let Ok(last_run) = DateTime::parse_from_rfc3339(last_run) else {
        return true;
    };
    Utc::now().signed_duration_since(last_run.with_timezone(&Utc))
        >= Duration::hours(AUTODREAM_MIN_INTERVAL_HOURS)
}

fn first_user_request_line(content: &str) -> &str {
    content
        .split("\n\n---\n\n")
        .next()
        .unwrap_or(content)
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or(content)
        .trim()
}

fn extract_path_mentions(content: &str) -> Vec<String> {
    let mut paths = Vec::new();
    for token in content.split_whitespace() {
        let token = token.trim_matches(|c: char| {
            matches!(
                c,
                '"' | '\'' | '`' | ',' | '.' | ':' | ';' | ')' | '(' | '[' | ']'
            )
        });
        if token.contains('/')
            || token.ends_with(".rs")
            || token.ends_with(".toml")
            || token.ends_with(".md")
            || token.ends_with(".json")
        {
            push_unique_capped(&mut paths, token, 120, 30);
        }
    }
    paths
}

fn push_unique_capped(items: &mut Vec<String>, value: &str, max_chars: usize, max_items: usize) {
    let value = value.trim();
    if value.is_empty() || items.len() >= max_items {
        return;
    }
    let capped = cap_chars(value, max_chars);
    if !items.iter().any(|item| item == &capped) {
        items.push(capped);
    }
}

fn append_bullets(out: &mut String, items: &[String]) {
    if items.is_empty() {
        out.push_str("- No durable observations gathered.\n");
        return;
    }
    for item in items {
        out.push_str("- ");
        out.push_str(item);
        out.push('\n');
    }
}

fn prune_file_lines_and_bytes(path: &Path, max_lines: usize, max_bytes: usize) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    let content = fs::read_to_string(path)
        .with_context(|| format!("Failed to read memory file {}", path.display()))?;
    if content.lines().count() <= max_lines && content.len() <= max_bytes {
        return Ok(());
    }

    let mut out = String::new();
    for line in content.lines().take(max_lines) {
        let next = if out.is_empty() {
            line.to_string()
        } else {
            format!("\n{line}")
        };
        if out.len() + next.len() > max_bytes {
            break;
        }
        out.push_str(&next);
    }
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str("[autoDream pruned older memory to stay under budget.]\n");
    if out.len() > max_bytes {
        out.truncate(max_bytes);
    }
    fs::write(path, out)
        .with_context(|| format!("Failed to prune memory file {}", path.display()))?;
    Ok(())
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

    #[test]
    fn autodream_waits_for_five_completed_sessions_then_consolidates() {
        let root = temp_root("autodream");
        let messages = root.join("messages");

        for idx in 0..4 {
            let store = MemoryStore::with_project_root(&root, &messages, format!("session-{idx}"));
            append_user_transcript(&store, &format!("Inspect src/lib.rs for session {idx}"));
            let outcome = store.complete_session_and_autodream().unwrap();
            assert_eq!(
                outcome,
                AutoDreamOutcome::Skipped(AutoDreamSkipReason::NotEnoughSessions)
            );
        }

        let store = MemoryStore::with_project_root(&root, &messages, "session-4");
        append_user_transcript(&store, "Update Cargo.toml and run cargo test");
        let outcome = store.complete_session_and_autodream().unwrap();

        assert!(matches!(
            outcome,
            AutoDreamOutcome::Consolidated {
                sessions: 5,
                transcript_matches: 5,
                ..
            }
        ));
        assert!(store
            .read_topic(AUTODREAM_TOPIC)
            .unwrap()
            .contains("Recent User Requests"));
        assert!(store
            .read_core_index()
            .unwrap()
            .unwrap()
            .contains("topics/autodream.md"));
        let state = store.load_autodream_state().unwrap();
        assert!(state.sessions_since_last_run.is_empty());
        assert!(state.last_run.is_some());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn autodream_respects_recent_run_gate() {
        let root = temp_root("autodream-recent");
        let store = MemoryStore::with_project_root(&root, root.join("messages"), "session");
        store
            .save_autodream_state(&AutoDreamState {
                last_run: Some(Utc::now().to_rfc3339()),
                sessions_since_last_run: (0..5).map(|idx| format!("session-{idx}")).collect(),
            })
            .unwrap();

        let outcome = store.run_autodream_if_due().unwrap();

        assert_eq!(
            outcome,
            AutoDreamOutcome::Skipped(AutoDreamSkipReason::RanRecently)
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn autodream_respects_lock_gate() {
        let root = temp_root("autodream-lock");
        let store = MemoryStore::with_project_root(&root, root.join("messages"), "session");
        fs::create_dir_all(store.auto_dream_lock_path().parent().unwrap()).unwrap();
        fs::write(store.auto_dream_lock_path(), "locked").unwrap();
        store
            .save_autodream_state(&AutoDreamState {
                last_run: None,
                sessions_since_last_run: (0..5).map(|idx| format!("session-{idx}")).collect(),
            })
            .unwrap();

        let outcome = store.run_autodream_if_due().unwrap();

        assert_eq!(
            outcome,
            AutoDreamOutcome::Skipped(AutoDreamSkipReason::Locked)
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn prune_file_keeps_lines_and_bytes_bounded() {
        let root = temp_root("autodream-prune");
        let path = root.join("memory.md");
        fs::write(
            &path,
            (0..500).map(|i| format!("line {i}\n")).collect::<String>(),
        )
        .unwrap();

        prune_file_lines_and_bytes(&path, 20, 200).unwrap();

        let pruned = fs::read_to_string(&path).unwrap();
        assert!(pruned.lines().count() <= 21);
        assert!(pruned.len() <= 200);

        let _ = fs::remove_dir_all(root);
    }

    fn append_user_transcript(store: &MemoryStore, content: &str) {
        store
            .append_transcript_message(&Message {
                role: "user".to_string(),
                content: Some(content.to_string()),
                tool_calls: None,
                tool_call_id: None,
            })
            .unwrap();
    }
}
