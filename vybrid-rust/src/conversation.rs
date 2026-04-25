use crate::client::groq::Message;

const MAX_TOOL_RESULT_CHARS: usize = 96 * 1024;
const REQUEST_CONTEXT_TOKEN_BUDGET: u32 = 110_000;
const RECENT_MESSAGE_FLOOR: usize = 40;

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
            .any(|m| m.content.as_deref().unwrap_or("").contains("omitted")));
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

    /// Request payload with a simple rolling window when history grows large.
    pub fn messages_for_request(&self) -> Vec<Message> {
        if self.estimate_context_tokens() <= REQUEST_CONTEXT_TOKEN_BUDGET
            || self.messages.len() <= RECENT_MESSAGE_FLOOR + 1
        {
            return self.get_messages();
        }

        let mut messages = Vec::new();
        if let Some(system_msg) = self.messages.first().cloned() {
            messages.push(system_msg);
        }

        let omitted = self.messages.len().saturating_sub(RECENT_MESSAGE_FLOOR + 1);
        messages.push(Message {
            role: "user".to_string(),
            content: Some(format!(
                "[Vybrid context summary] {omitted} earlier message(s) were omitted to stay within the model context window. Keep using the visible recent tool results, compiler spans, and file contents as authoritative context."
            )),
            tool_calls: None,
            tool_call_id: None,
        });

        let start = self.messages.len().saturating_sub(RECENT_MESSAGE_FLOOR);
        messages.extend(self.messages[start..].iter().cloned());
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
            if let Some(c) = &m.content {
                chars += c.len();
            }
            if let Some(tcs) = &m.tool_calls {
                for tc in tcs {
                    chars += tc.id.len();
                    chars += tc.function.name.len();
                    chars += tc.function.arguments.len();
                }
            }
            if let Some(id) = &m.tool_call_id {
                chars += id.len();
            }
        }
        chars.div_ceil(4).min(u32::MAX as usize) as u32
    }
}
