use crate::client::glm::Message;

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
        self.messages.push(Message {
            role: "tool".to_string(),
            content: Some(result.to_string()),
            tool_calls: None,
            tool_call_id: Some(tool_call_id.to_string()),
        });
    }

    pub fn get_messages(&self) -> Vec<Message> {
        self.messages.clone()
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
        ((chars + 3) / 4).min(u32::MAX as usize) as u32
    }
}
