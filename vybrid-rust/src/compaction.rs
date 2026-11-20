//! Provider summaries and a recoverable local fallback share the same history boundaries.
use crate::client::groq::{GroqClient, Message};
use crate::conversation::{Conversation, COMPACT_KEEP_RECENT, COMPACT_MIN_TO_SUMMARIZE};
use crate::tools::output::truncate_utf8_middle;
use anyhow::{Context, Result};
use std::fs;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub const DEFAULT_FOCUS: &str =
    "Preserve the task, constraints, decisions, exact paths, verification results and remaining work.";
const SUMMARY_TIMEOUT: Duration = Duration::from_secs(20);
const SUMMARY_BACKOFF: Duration = Duration::from_secs(60);

pub enum Compaction {
    Summarized,
    Archived(PathBuf),
    Unchanged,
}

impl Compaction {
    pub fn status(&self, before: u32, after: u32) -> String {
        match self {
            Self::Summarized => format!("Summarized older messages (~{before} → ~{after} active tokens)."),
            Self::Archived(path) => format!("Compacted locally (~{before} → ~{after} active tokens). Full older messages in {}.", path.display()),
            Self::Unchanged => "No complete older range could be reduced. History preserved.".into(),
        }
    }
}

pub struct Compactor {
    archive_dir: PathBuf,
    retry_at: Option<Instant>,
}

impl Compactor {
    pub fn new(progress_dir: PathBuf) -> Self {
        Self {
            archive_dir: progress_dir.join("compactions"),
            retry_at: None,
        }
    }

    pub async fn compact(
        &mut self,
        client: Option<&GroqClient>,
        conversation: &mut Conversation,
        focus: &str,
    ) -> Result<Compaction> {
        let end = conversation.first_recent_group(COMPACT_KEEP_RECENT);
        if end <= 1 {
            return Ok(Compaction::Unchanged);
        }
        if let Some(client) = client.filter(|_| self.retry_at.is_none_or(|at| Instant::now() >= at))
        {
            if let Some((covered, transcript)) =
                conversation.compactable_transcript(COMPACT_KEEP_RECENT)
            {
                match tokio::time::timeout(
                    SUMMARY_TIMEOUT,
                    request_summary(client, transcript, focus),
                )
                .await
                {
                    Ok(Ok(Some(summary))) => {
                        if conversation.apply_manual_compaction(&summary, covered) {
                            return Ok(Compaction::Summarized);
                        }
                    }
                    _ => self.retry_at = Some(Instant::now() + SUMMARY_BACKOFF),
                }
            }
        }

        let older = &conversation.messages[1..end];
        if older.len() < COMPACT_MIN_TO_SUMMARIZE
            && older
                .iter()
                .map(crate::conversation::estimate_message_tokens)
                .max()
                .unwrap_or(0)
                < 512
        {
            return Ok(Compaction::Unchanged);
        }
        let path = self
            .archive_dir
            .join(format!("{}.jsonl", uuid::Uuid::new_v4()));
        let excerpt = archive_excerpt(older, &path);
        if conversation.apply_archived_excerpt(&excerpt, end, |messages| {
            archive_messages(&self.archive_dir, &path, messages)
        })? {
            Ok(Compaction::Archived(path))
        } else {
            Ok(Compaction::Unchanged)
        }
    }
}

async fn request_summary(
    client: &GroqClient,
    transcript: String,
    focus: &str,
) -> Result<Option<String>> {
    // Reserve less of the provider's token window for short histories.
    let limit = (transcript.len() / 8).clamp(512, 2_048) as u32;
    let request = [
        Message {
            role: "system".into(),
            content: Some("Summarize older coding-agent history in at most eight short bullets. Preserve exact paths, errors, decisions and unfinished work. Only report facts in the transcript.".into()),
            tool_calls: None, tool_call_id: None,
        },
        Message {
            role: "user".into(),
            content: Some(format!("Focus: {focus}\n\nTranscript:\n{transcript}")),
            tool_calls: None, tool_call_id: None,
        },
    ];
    let response = client
        .with_completion_limit(limit)
        .chat(&request, None)
        .await?;
    Ok(response.content.filter(|s| !s.trim().is_empty()))
}

fn archive_excerpt(older: &[Message], path: &Path) -> String {
    let mut excerpt = format!("Archived {} older messages. Read exact messages and tool results with read_file at `{}`. Recent excerpts from this range:\n", older.len(), path.display());
    for message in &older[older.len().saturating_sub(6)..] {
        if let Some(calls) = &message.tool_calls {
            for call in calls.iter().take(3) {
                excerpt.push_str(&format!(
                    "Tool {} {}\n",
                    call.function.name,
                    truncate_utf8_middle(&call.function.arguments, 140, "arguments")
                ));
            }
        }
        if let Some(content) = message.content.as_deref().filter(|s| !s.is_empty()) {
            excerpt.push_str(&format!(
                "[{}] {}\n",
                message.role,
                truncate_utf8_middle(content, 320, "excerpt")
            ));
        }
    }
    excerpt
}

// One complete message per line lets read_file retrieve exact ranges, including
// tool IDs and arguments. Publish the path to the model only after a successful flush.
fn archive_messages(directory: &Path, path: &Path, messages: &[Message]) -> Result<()> {
    fs::create_dir_all(directory)
        .with_context(|| format!("Could not create {}", directory.display()))?;
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options
        .open(path)
        .with_context(|| format!("Could not create {}", path.display()))?;
    let result = (|| -> Result<()> {
        let mut writer = BufWriter::new(file);
        for message in messages {
            serde_json::to_writer(&mut writer, message)?;
            writer.write_all(b"\n")?;
        }
        writer.flush()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(path);
    }
    result.with_context(|| format!("Could not save {}", path.display()))
}
