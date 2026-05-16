use crate::client::groq::Message;

const MAX_TOOL_RESULT_CHARS: usize = 48 * 1024;
#[allow(dead_code)]
pub const REQUEST_CONTEXT_TOKEN_BUDGET: u32 = 36_000;
const MIN_COMPACTED_MESSAGE_CHARS: usize = 2_048;

fn truncate_middle(content: &str, max_chars: usize, label: &str) -> String {
    if content.chars().count() <= max_chars {
        return content.to_string();
    }
    let half = max_chars / 2;
    let head: String = content.chars().take(half).collect();
    let tail: String = content
        .chars()
        .rev()
        .take(half)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!("{head}\n\n[{label} truncated: showing head and tail]\n\n{tail}",)
}

fn estimate_message_tokens(message: &Message) -> u32 {
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

fn compact_message_for_request(message: &Message, max_chars: usize) -> Message {
    let mut compacted = message.clone();
    if let Some(content) = &message.content {
        compacted.content = Some(truncate_middle(content, max_chars, "message"));
    }

    if let Some(tool_calls) = &message.tool_calls {
        compacted.tool_calls = Some(
            tool_calls
                .iter()
                .map(|tc| {
                    let mut tc = tc.clone();
                    tc.function.arguments = truncate_middle(
                        &tc.function.arguments,
                        max_chars.min(8 * 1024),
                        "tool arguments",
                    );
                    tc
                })
                .collect(),
        );
    }

    compacted
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_results_are_truncated_before_storage() {
        let mut conversation = Conversation::new("system");
        let large = "x".repeat(MAX_TOOL_RESULT_CHARS + 1024);
        conversation.add_tool_result("call-1", &large);

        let stored = conversation
            .messages
            .last()
            .unwrap()
            .content
            .as_ref()
            .unwrap();
        assert!(stored.contains("tool result"));
        assert!(stored.len() < large.len());
    }

    #[test]
    fn request_messages_keep_system_and_recent_messages() {
        let mut conversation = Conversation::new(&"s".repeat(16_000));
        for i in 0..120 {
            conversation.add_user_message(&format!("message-{i}: {}", "x".repeat(4_000)));
        }

        let request = conversation.messages_for_request();
        assert_eq!(request.first().unwrap().role, "system");
        assert!(request.len() < conversation.messages.len());
        assert!(request
            .iter()
            .any(|m| m.content.as_deref().unwrap_or("").contains("compacted")));
    }
}

/// Manages conversation history
#[derive(Debug, Clone)]
pub struct Conversation {
    pub messages: Vec<Message>,
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

    pub fn add_tool_result(&mut self, tool_call_id: &str, result: &str) {
        let result = truncate_middle(result, MAX_TOOL_RESULT_CHARS, "tool result");
        self.messages.push(Message {
            role: "tool".to_string(),
            content: Some(result),
            tool_calls: None,
            tool_call_id: Some(tool_call_id.to_string()),
        });
    }

    pub fn get_messages(&self) -> Vec<Message> {
        self.messages.clone()
    }

    /// Request payload with adaptive compaction when history grows large.
    #[allow(dead_code)]
    pub fn messages_for_request(&self) -> Vec<Message> {
        self.messages_for_request_with_budget(REQUEST_CONTEXT_TOKEN_BUDGET)
    }

    /// Request payload that keeps the system prompt and the newest complete messages within budget.
    pub fn messages_for_request_with_budget(&self, token_budget: u32) -> Vec<Message> {
        if self.estimate_context_tokens() <= token_budget {
            return self.get_messages();
        }

        let mut messages = Vec::new();
        let mut used_tokens = 0u32;
        if let Some(system_msg) = self.messages.first().cloned() {
            used_tokens = used_tokens.saturating_add(estimate_message_tokens(&system_msg));
            messages.push(system_msg);
        }

        let summary = Message {
            role: "user".to_string(),
            content: Some(format!(
                "[Vybrid context summary] Earlier messages were compacted to stay within the model context window. Keep using the visible recent tool results, compiler spans, and file contents as authoritative context."
            )),
            tool_calls: None,
            tool_call_id: None,
        };
        used_tokens = used_tokens.saturating_add(estimate_message_tokens(&summary));
        messages.push(summary);

        let mut selected = Vec::new();
        let mut remaining_tokens = token_budget.saturating_sub(used_tokens);

        for message in self.messages.iter().skip(1).rev() {
            if remaining_tokens == 0 {
                break;
            }

            let message_tokens = estimate_message_tokens(message);
            if message_tokens <= remaining_tokens {
                selected.push(message.clone());
                remaining_tokens = remaining_tokens.saturating_sub(message_tokens);
                continue;
            }

            let remaining_chars = (remaining_tokens as usize).saturating_mul(4);
            if remaining_chars >= MIN_COMPACTED_MESSAGE_CHARS {
                let compacted = compact_message_for_request(message, remaining_chars);
                let compacted_tokens = estimate_message_tokens(&compacted);
                if compacted_tokens <= remaining_tokens {
                    selected.push(compacted);
                }
            }
            break;
        }

        selected.reverse();

        // OpenAI-compatible APIs reject orphan tool messages whose matching assistant call was
        // outside the window. Drop any such leading tool messages after compaction.
        while selected.first().map(|m| m.role.as_str()) == Some("tool") {
            selected.remove(0);
        }

        messages.extend(selected);
        messages
    }

    pub fn clear_keeping_system(&mut self) {
        if let Some(system_msg) = self.messages.first().cloned() {
            if system_msg.role == "system" {
                self.messages = vec![system_msg];
                return;
            }
        }
        self.messages.clear();
    }

    /// Rough token estimate for the context meter (~4 chars per token for mixed text/code).
    pub fn estimate_context_tokens(&self) -> u32 {
        let mut chars: usize = 0;
        for m in &self.messages {
            chars += estimate_message_tokens(m) as usize * 4;
        }
        chars.div_ceil(4).min(u32::MAX as usize) as u32
    }
}
