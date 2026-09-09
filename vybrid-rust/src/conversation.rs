use crate::client::groq::Message;
use std::borrow::Cow;
use std::collections::BTreeMap;

#[allow(dead_code)]
pub const REQUEST_CONTEXT_TOKEN_BUDGET: u32 = 36_000;

/// Stable marker inserted when older messages are dropped from the request window.
/// Its content must never vary between requests: Groq prefix-caches identical
/// request prefixes (system prompt, tools, leading messages), so any churn here
/// would invalidate the cache on every call.
const COMPACTION_MARKER: &str = "[Vybrid context summary] Earlier messages were compacted to stay within the model context window. Keep using the visible recent tool results, compiler spans, and file contents as authoritative context.";

pub const MANUAL_COMPACTION_PREFIX: &str = "[Vybrid compaction summary]";
pub const COMPACT_KEEP_RECENT: usize = 8;
pub const COMPACT_MIN_TO_SUMMARIZE: usize = 4;
const COMPACT_TRANSCRIPT_MAX_CHARS: usize = 24_000;

fn compaction_marker_message() -> Message {
    Message {
        role: "user".to_string(),
        content: Some(COMPACTION_MARKER.to_string()),
        tool_calls: None,
        tool_call_id: None,
    }
}

/// Keep the head and tail of oversized content. Single forward/backward scan over
/// char boundaries; avoids intermediate `Vec<char>` allocations.
#[cfg(test)]
pub(crate) fn truncate_middle(content: &str, max_chars: usize, label: &str) -> String {
    let total_chars = content.chars().count();
    if total_chars <= max_chars {
        return content.to_string();
    }
    let half = max_chars / 2;
    let head_end = content
        .char_indices()
        .nth(half)
        .map(|(idx, _)| idx)
        .unwrap_or(content.len());
    let tail_start = if half == 0 {
        content.len()
    } else {
        content
            .char_indices()
            .rev()
            .nth(half - 1)
            .map(|(idx, _)| idx)
            .unwrap_or(0)
    };
    format!(
        "{}\n\n[{label} truncated: showing head and tail]\n\n{}",
        &content[..head_end],
        &content[tail_start..]
    )
}

pub(crate) fn estimate_message_tokens(message: &Message) -> u32 {
    let mut chars: usize = 0;
    if let Some(c) = &message.content {
        chars += c.len();
    }
    if let Some(tcs) = &message.tool_calls {
        for tc in tcs {
            chars += tc.id.len();
            chars += tc.function.name.len();
            chars += tc.function.arguments.len();
        }
    }
    if let Some(id) = &message.tool_call_id {
        chars += id.len();
    }
    chars.div_ceil(4).min(u32::MAX as usize) as u32
}

/// Manages conversation history
#[derive(Debug, Clone)]
pub struct Conversation {
    pub messages: Vec<Message>,
    /// Index of the first non-system message included in requests. Monotonically
    /// non-decreasing within a conversation so consecutive requests share an
    /// identical prefix (system prompt + marker + same leading messages), which
    /// maximizes Groq prompt-cache hits.
    request_floor: usize,
    pinned: BTreeMap<String, Message>,
    next_task_id: u64,
}

impl Conversation {
    pub fn new(system_prompt: &str) -> Self {
        Self {
            messages: vec![Message {
                role: "system".to_string(),
                content: Some(system_prompt.to_string()),
                tool_calls: None,
                tool_call_id: None,
            }],
            request_floor: 1,
            pinned: BTreeMap::new(),
            next_task_id: 0,
        }
    }

    pub fn add_user_message(&mut self, content: &str) {
        self.messages.push(Message {
            role: "user".to_string(),
            content: Some(content.to_string()),
            tool_calls: None,
            tool_call_id: None,
        });
    }

    pub fn add_assistant_message(&mut self, message: Message) {
        self.messages.push(message);
    }

    /// User-authored task text survives automatic windowing. Corrective harness
    /// notes deliberately use add_user_message instead of replacing this anchor.
    pub fn add_task_message(&mut self, content: &str) {
        self.add_user_message(content);
        let message = self.messages.last().unwrap().clone();
        self.next_task_id += 1;
        self.pinned.insert(
            format!("task_history:{:020}", self.next_task_id),
            message.clone(),
        );
        self.pinned
            .entry("task_origin".into())
            .or_insert_with(|| message.clone());
        self.pinned.insert("task_latest".into(), message);
    }

    /// Append a context revision only when it changes; never rewrite a live prefix.
    pub fn set_context_snapshot(&mut self, key: &str, content: &str) -> bool {
        if self.pinned.get(key).and_then(|m| m.content.as_deref()) == Some(content) {
            return false;
        }
        self.add_user_message(content);
        self.pinned
            .insert(key.into(), self.messages.last().unwrap().clone());
        true
    }

    fn missing_pins(&self, floor: usize) -> Vec<&Message> {
        let tail = &self.messages[floor.min(self.messages.len())..];
        let mut missing: Vec<&Message> = Vec::new();
        for pin in self.pinned.values() {
            if !tail
                .iter()
                .chain(missing.iter().copied())
                .any(|m| m.role == pin.role && m.content == pin.content)
            {
                missing.push(pin);
            }
        }
        missing
    }

    fn tokens_from(&self, floor: usize) -> u32 {
        self.messages
            .first()
            .map(estimate_message_tokens)
            .unwrap_or(0)
            .saturating_add(if floor > 1 { 64 } else { 0 })
            .saturating_add(
                self.messages[floor.min(self.messages.len())..]
                    .iter()
                    .chain(self.missing_pins(floor))
                    .map(estimate_message_tokens)
                    .fold(0, u32::saturating_add),
            )
    }

    /// End of an indivisible assistant/tool-result group.
    fn group_end(&self, start: usize) -> usize {
        let mut end = start + 1;
        while end < self.messages.len() && self.messages[end].role == "tool" {
            end += 1;
        }
        end
    }

    pub fn estimate_active_tokens(&self) -> u32 {
        self.tokens_from(self.request_floor.max(1))
    }

    pub fn add_tool_result(&mut self, tool_call_id: &str, result: &str) {
        self.messages.push(Message {
            role: "tool".to_string(),
            content: Some(result.to_owned()),
            tool_calls: None,
            tool_call_id: Some(tool_call_id.to_string()),
        });
    }

    #[allow(dead_code)]
    pub fn get_messages(&self) -> Vec<Message> {
        self.messages.clone()
    }

    /// Request payload with adaptive compaction when history grows large.
    #[allow(dead_code)]
    pub fn messages_for_request(&mut self) -> Cow<'_, [Message]> {
        self.messages_for_request_with_budget(REQUEST_CONTEXT_TOKEN_BUDGET)
    }

    /// Request payload that keeps the system prompt and the newest complete messages
    /// within budget. Borrows the full history when no compaction is needed (no clone),
    /// and uses a sticky floor so consecutive requests share a cacheable prefix.
    pub fn messages_for_request_with_budget(&mut self, token_budget: u32) -> Cow<'_, [Message]> {
        self.advance_floor_to_fit(token_budget);

        if self.request_floor <= 1 {
            return Cow::Borrowed(&self.messages);
        }

        let tail = &self.messages[self.request_floor.min(self.messages.len())..];
        let mut out = Vec::with_capacity(tail.len() + 2);
        if let Some(system) = self.messages.first() {
            if system.role == "system" {
                out.push(system.clone());
            }
        }
        out.push(compaction_marker_message());
        out.extend(self.missing_pins(self.request_floor).into_iter().cloned());
        out.extend(tail.iter().cloned());
        Cow::Owned(out)
    }

    /// Advance the request floor (never backward) until the request fits the budget.
    fn advance_floor_to_fit(&mut self, token_budget: u32) {
        self.request_floor = self.request_floor.max(1);
        if self.tokens_from(self.request_floor) <= token_budget {
            return;
        }
        let target = token_budget.saturating_mul(3) / 4;
        while self.request_floor < self.messages.len()
            && self.tokens_from(self.request_floor) > target
        {
            let next = self.group_end(self.request_floor);
            // A context policy budget is soft: keep the newest complete group,
            // even when it alone exceeds the target, instead of sending orphans.
            if next >= self.messages.len() {
                break;
            }
            self.request_floor = next;
        }
    }

    pub fn clear_keeping_system(&mut self) {
        self.request_floor = 1;
        self.pinned.clear();
        self.next_task_id = 0;
        if let Some(system_msg) = self.messages.first().cloned() {
            if system_msg.role == "system" {
                self.messages = vec![system_msg];
                return;
            }
        }
        self.messages.clear();
    }

    pub fn set_system_prompt(&mut self, prompt: &str) {
        self.request_floor = 1;
        if let Some(system) = self.messages.first_mut() {
            if system.role == "system" {
                system.content = Some(prompt.to_string());
                return;
            }
        }
        self.messages.insert(
            0,
            Message {
                role: "system".to_string(),
                content: Some(prompt.to_string()),
                tool_calls: None,
                tool_call_id: None,
            },
        );
    }

    /// Returns `(first_kept_index, transcript_text)` for manual compaction, or None if
    /// there is too little history to summarize.
    pub fn compactable_transcript(&self, keep_recent: usize) -> Option<(usize, String)> {
        if self.messages.len() <= 1 {
            return None;
        }

        let last_idx = self.messages.len().saturating_sub(1);
        let mut first_kept = last_idx.saturating_sub(keep_recent.saturating_sub(1));
        if first_kept <= 1 {
            return None;
        }

        while first_kept < last_idx && self.messages[first_kept].role == "tool" {
            first_kept += 1;
        }

        let to_summarize = first_kept.saturating_sub(1);
        if to_summarize < COMPACT_MIN_TO_SUMMARIZE {
            return None;
        }

        let mut transcript = String::new();
        let mut covered = 1;
        while covered < first_kept {
            let end = self.group_end(covered);
            if end > first_kept {
                break;
            }
            let mut group = String::new();
            for message in &self.messages[covered..end] {
                group.push_str(&format!(
                    "[{}] {}\n",
                    message.role,
                    message.content.as_deref().unwrap_or("")
                ));
                if let Some(calls) = &message.tool_calls {
                    for call in calls {
                        group.push_str(&format!(
                            "[tool call {}] {} {}\n",
                            call.id, call.function.name, call.function.arguments
                        ));
                    }
                }
                group.push('\n');
            }
            // Never delete a group whose complete text was not provided.
            if transcript.len() + group.len() > COMPACT_TRANSCRIPT_MAX_CHARS {
                break;
            }
            transcript.push_str(&group);
            covered = end;
        }
        first_kept = covered;

        if transcript.trim().is_empty() || first_kept.saturating_sub(1) < COMPACT_MIN_TO_SUMMARIZE {
            return None;
        }

        Some((first_kept, transcript))
    }

    pub fn apply_manual_compaction(&mut self, summary: &str, first_kept_index: usize) -> bool {
        if summary.trim().is_empty()
            || first_kept_index <= 1
            || first_kept_index >= self.messages.len()
            || self.messages[first_kept_index].role == "tool"
        {
            return false;
        }

        let summary_message = Message {
            role: "user".to_string(),
            content: Some(format!("{MANUAL_COMPACTION_PREFIX} {summary}")),
            tool_calls: None,
            tool_call_id: None,
        };

        let mut next = vec![self.messages[0].clone(), summary_message.clone()];
        let covered_tasks: Vec<_> = self
            .pinned
            .iter()
            .filter(|(key, message)| {
                key.starts_with("task_history:")
                    && self.messages[1..first_kept_index]
                        .iter()
                        .any(|m| m.content == message.content)
            })
            .map(|(key, message)| (key.clone(), message.content.clone()))
            .collect();
        let mut pins = self.missing_pins(first_kept_index);
        pins.retain(|message| {
            self.pinned
                .get("task_origin")
                .is_some_and(|m| m.content == message.content)
                || self
                    .pinned
                    .get("task_latest")
                    .is_some_and(|m| m.content == message.content)
                || !covered_tasks
                    .iter()
                    .any(|(_, content)| *content == message.content)
        });
        if let Some(old_summary) = self.pinned.get("summary") {
            pins.retain(|m| m.content != old_summary.content);
        }
        next.extend(pins.into_iter().cloned());
        next.extend_from_slice(&self.messages[first_kept_index..]);
        let next_tokens = next
            .iter()
            .map(estimate_message_tokens)
            .fold(0, u32::saturating_add);
        if next_tokens >= self.estimate_context_tokens() {
            return false;
        }
        self.messages = next;
        for (key, _) in covered_tasks {
            self.pinned.remove(&key);
        }
        self.pinned.insert("summary".into(), summary_message);
        self.request_floor = 1;
        true
    }

    /// Rough token estimate for the context meter (~4 chars per token for mixed text/code).
    pub fn estimate_context_tokens(&self) -> u32 {
        self.messages
            .iter()
            .map(estimate_message_tokens)
            .fold(0u32, |acc, t| acc.saturating_add(t))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_results_preserve_the_presented_range() {
        let mut conversation = Conversation::new("system");
        let large = "x".repeat(64 * 1024);
        conversation.add_tool_result("call-1", &large);

        let stored = conversation
            .messages
            .last()
            .unwrap()
            .content
            .as_ref()
            .unwrap();
        assert_eq!(stored, &large);
    }

    #[test]
    fn truncate_middle_keeps_head_and_tail() {
        let content: String = ('a'..='z').cycle().take(1_000).collect();
        let truncated = truncate_middle(&content, 100, "test");

        assert!(truncated.starts_with(&content[..50]));
        assert!(truncated.ends_with(&content[content.len() - 50..]));
        assert!(truncated.contains("truncated"));
    }

    #[test]
    fn request_messages_keep_system_and_recent_messages() {
        let mut conversation = Conversation::new(&"s".repeat(16_000));
        for i in 0..120 {
            conversation.add_user_message(&format!("message-{i}: {}", "x".repeat(4_000)));
        }

        let total_messages = conversation.messages.len();
        let request = conversation.messages_for_request();
        assert_eq!(request.first().unwrap().role, "system");
        assert!(request.len() < total_messages);
        assert!(request
            .iter()
            .any(|m| m.content.as_deref().unwrap_or("").contains("compacted")));
    }

    #[test]
    fn under_budget_requests_borrow_without_compaction() {
        let mut conversation = Conversation::new("system");
        conversation.add_user_message("hello");

        let request = conversation.messages_for_request_with_budget(10_000);
        assert!(matches!(request, Cow::Borrowed(_)));
        assert_eq!(request.len(), 2);
    }

    #[test]
    fn compaction_floor_is_sticky_for_stable_prefixes() {
        let mut conversation = Conversation::new("system");
        for i in 0..40 {
            conversation.add_user_message(&format!("message-{i}: {}", "x".repeat(2_000)));
        }

        let first: Vec<Message> = conversation
            .messages_for_request_with_budget(8_000)
            .to_vec();
        let second: Vec<Message> = conversation
            .messages_for_request_with_budget(8_000)
            .to_vec();

        // Same budget, no new messages: requests must be byte-identical for cache hits.
        let first_json = serde_json::to_string(&first).unwrap();
        let second_json = serde_json::to_string(&second).unwrap();
        assert_eq!(first_json, second_json);

        // New message appended: previous prefix must be preserved exactly.
        conversation.add_user_message("newest");
        let third: Vec<Message> = conversation
            .messages_for_request_with_budget(8_000)
            .to_vec();
        for (a, b) in second.iter().zip(third.iter()) {
            assert_eq!(
                serde_json::to_string(a).unwrap(),
                serde_json::to_string(b).unwrap()
            );
        }
        assert_eq!(third.len(), second.len() + 1);
    }

    #[test]
    fn compaction_skips_orphan_tool_messages() {
        let mut conversation = Conversation::new("system");
        for i in 0..30 {
            conversation.add_user_message(&format!("message-{i}: {}", "x".repeat(2_000)));
            conversation.add_tool_result(&format!("call-{i}"), &"y".repeat(2_000));
        }

        let request = conversation.messages_for_request_with_budget(6_000);
        let first_non_marker = request.get(2).map(|m| m.role.clone()).unwrap_or_default();
        assert_ne!(first_non_marker, "tool");
    }

    #[test]
    fn compactable_transcript_requires_enough_history() {
        let mut conversation = Conversation::new("system");
        for i in 0..3 {
            conversation.add_user_message(&format!("msg-{i}"));
        }
        assert!(conversation
            .compactable_transcript(COMPACT_KEEP_RECENT)
            .is_none());
    }

    #[test]
    fn compactable_transcript_returns_middle_messages() {
        let mut conversation = Conversation::new("system");
        for i in 0..20 {
            conversation.add_user_message(&format!("message-{i}"));
            conversation.add_tool_result(&format!("call-{i}"), "tool output");
        }

        let (first_kept, transcript) = conversation
            .compactable_transcript(COMPACT_KEEP_RECENT)
            .expect("expected compactable transcript");
        assert!(first_kept > 1);
        assert!(transcript.contains("[user]"));
        assert!(transcript.contains("message-0"));
    }

    #[test]
    fn apply_manual_compaction_replaces_middle_and_resets_floor() {
        let mut conversation = Conversation::new("system");
        for i in 0..20 {
            conversation.add_user_message(&format!("message-{i}"));
        }
        conversation.request_floor = 5;

        let (first_kept, _) = conversation
            .compactable_transcript(COMPACT_KEEP_RECENT)
            .expect("expected compactable transcript");
        let before = conversation.messages.len();
        conversation.apply_manual_compaction("summary of earlier work", first_kept);

        assert_eq!(conversation.request_floor, 1);
        assert!(conversation.messages.len() < before);
        assert!(conversation.messages[1]
            .content
            .as_deref()
            .unwrap_or("")
            .contains(MANUAL_COMPACTION_PREFIX));
        assert!(conversation
            .messages
            .last()
            .unwrap()
            .content
            .as_deref()
            .unwrap_or("")
            .contains("message-19"));
    }

    #[test]
    fn set_system_prompt_replaces_system_message() {
        let mut conversation = Conversation::new("old prompt");
        conversation.request_floor = 4;
        conversation.add_user_message("hello");
        conversation.set_system_prompt("new prompt");
        assert_eq!(
            conversation.messages[0].content.as_deref(),
            Some("new prompt")
        );
        assert_eq!(conversation.request_floor, 1);
    }
}
