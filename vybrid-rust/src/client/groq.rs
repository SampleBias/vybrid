#![allow(dead_code)]

use anyhow::{Context, Result};
use futures::StreamExt;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::pin::Pin;

/// Groq OpenAI-compatible Chat Completions client (`https://api.groq.com/openai/v1`).
#[derive(Debug, Clone)]
pub struct GroqClient {
    client: Client,
    api_key: String,
    base_url: String,
    model: String,
}

/// Chat completion request (OpenAI-compatible subset).
#[derive(Debug, Serialize)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<Message>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<Tool>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<String>,
    pub stream: bool,
    pub max_tokens: u32,
    pub temperature: f32,
}

/// Message in conversation
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Message {
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

/// Tool call from assistant
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub call_type: String,
    pub function: FunctionCall,
}

/// Function call details
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct FunctionCall {
    pub name: String,
    pub arguments: String,
}

/// Tool definition
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Tool {
    #[serde(rename = "type")]
    pub tool_type: String,
    pub function: FunctionDef,
}

/// Function definition for tool
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct FunctionDef {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

/// Streaming response chunk
#[derive(Debug, Deserialize)]
pub struct StreamChunk {
    pub id: Option<String>,
    pub choices: Vec<StreamChoice>,
    pub usage: Option<Usage>,
}

/// Usage statistics
#[derive(Debug, Deserialize)]
pub struct Usage {
    pub prompt_tokens: Option<u32>,
    pub completion_tokens: Option<u32>,
    pub total_tokens: Option<u32>,
}

/// Stream choice
#[derive(Debug, Deserialize)]
pub struct StreamChoice {
    pub index: usize,
    pub delta: Delta,
    pub finish_reason: Option<String>,
}

/// Delta content in stream
#[derive(Debug, Deserialize, Default)]
pub struct Delta {
    pub role: Option<String>,
    pub content: Option<String>,
    pub reasoning_content: Option<String>,
    pub tool_calls: Option<Vec<ToolCallDelta>>,
}

/// Tool call delta in stream
#[derive(Debug, Deserialize)]
pub struct ToolCallDelta {
    pub index: usize,
    pub id: Option<String>,
    #[serde(rename = "type")]
    pub call_type: Option<String>,
    pub function: Option<FunctionDelta>,
}

/// Function delta in stream
#[derive(Debug, Deserialize)]
pub struct FunctionDelta {
    pub name: Option<String>,
    pub arguments: Option<String>,
}

/// Accumulated tool call during streaming
#[derive(Debug, Clone, Default)]
pub struct AccumulatedToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

impl GroqClient {
    pub fn new(api_key: String, base_url: String, model: String) -> Self {
        Self {
            client: Client::builder()
                .timeout(std::time::Duration::from_secs(300))
                .build()
                .expect("Failed to create HTTP client"),
            api_key,
            base_url,
            model,
        }
    }

    /// Stream chat completion response
    pub async fn chat_stream(
        &self,
        messages: Vec<Message>,
        tools: Option<Vec<Tool>>,
    ) -> Result<Pin<Box<dyn futures::Stream<Item = Result<StreamChunk>> + Send>>> {
        let request = ChatRequest {
            model: self.model.clone(),
            messages,
            tools,
            tool_choice: Some("auto".to_string()),
            stream: true,
            max_tokens: 8192,
            temperature: 1.0,
        };

        let response = self
            .client
            .post(format!("{}/chat/completions", self.base_url))
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .header("Accept", "text/event-stream")
            .json(&request)
            .send()
            .await
            .context("Failed to send request to Groq API")?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "Unknown error".to_string());
            anyhow::bail!("API error ({}): {}", status, body);
        }

        let stream = response.bytes_stream();

        let output_stream = async_stream::stream! {
            let mut buffer = String::new();

            futures::pin_mut!(stream);

            while let Some(chunk_result) = stream.next().await {
                match chunk_result {
                    Ok(bytes) => {
                        buffer.push_str(&String::from_utf8_lossy(&bytes));

                        while let Some(pos) = buffer.find("\n\n") {
                            let event = buffer[..pos].to_string();
                            buffer = buffer[pos + 2..].to_string();

                            for line in event.lines() {
                                if line.starts_with("data: ") {
                                    let data = &line[6..];

                                    let data_trim = data.trim();
                                    if data_trim == "[DONE]" {
                                        return;
                                    }

                                    // Groq may emit JSON error objects in the SSE stream (no `choices` field).
                                    if let Ok(v) = serde_json::from_str::<Value>(data_trim) {
                                        if v.get("error").is_some() {
                                            eprintln!("API error (stream): {}", data_trim);
                                            continue;
                                        }
                                    }

                                    match serde_json::from_str::<StreamChunk>(data_trim) {
                                        Ok(chunk) => yield Ok(chunk),
                                        Err(e) => {
                                            eprintln!("Parse warning: {} for data: {}", e, data_trim);
                                        }
                                    }
                                }
                            }
                        }
                    }
                    Err(e) => {
                        yield Err(anyhow::anyhow!("Stream error: {}", e));
                        return;
                    }
                }
            }
        };

        Ok(Box::pin(output_stream))
    }

    /// Non-streaming chat completion
    pub async fn chat(&self, messages: Vec<Message>, tools: Option<Vec<Tool>>) -> Result<Message> {
        let request = ChatRequest {
            model: self.model.clone(),
            messages,
            tools,
            tool_choice: Some("auto".to_string()),
            stream: false,
            max_tokens: 8192,
            temperature: 1.0,
        };

        let response = self
            .client
            .post(format!("{}/chat/completions", self.base_url))
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&request)
            .send()
            .await
            .context("Failed to send request to Groq API")?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "Unknown error".to_string());
            anyhow::bail!("API error ({}): {}", status, body);
        }

        #[derive(Deserialize)]
        struct ChatResponse {
            choices: Vec<ChatChoice>,
        }

        #[derive(Deserialize)]
        struct ChatChoice {
            message: Message,
        }

        let chat_response: ChatResponse = response
            .json()
            .await
            .context("Failed to parse API response")?;

        chat_response
            .choices
            .into_iter()
            .next()
            .map(|c| c.message)
            .context("No response from API")
    }
}
