#![allow(dead_code)]

use anyhow::{Context, Result};
use futures::StreamExt;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::pin::Pin;

/// Shorten repetitive Groq tool-schema validation messages for the terminal.
fn simplify_tool_validation_message(msg: &str) -> String {
    if !msg.contains("did not match schema") && !msg.contains("tool call validation") {
        return msg.to_string();
    }
    // Groq sometimes echoes the same phrase twice and lists every failed schema branch.
    let one_line: String = msg.split_whitespace().collect::<Vec<_>>().join(" ");
    if one_line.len() <= 280 {
        return one_line;
    }
    if let Some(idx) = one_line.find("parameters for tool") {
        let tail: String = one_line[idx..].chars().take(140).collect();
        return format!("Tool arguments did not match the schema ({tail}…).");
    }
    format!("{}…", one_line.chars().take(200).collect::<String>())
}

/// Turn a Groq/OpenAI-style SSE error JSON into a short user-facing message (no huge `failed_generation`).
fn stream_api_error_user_message(body: &Value) -> String {
    let Some(err) = body.get("error") else {
        return "The API returned an error while streaming.".to_string();
    };
    let raw_msg = err
        .get("message")
        .and_then(|m| m.as_str())
        .unwrap_or("Unknown API error");
    let msg = simplify_tool_validation_message(raw_msg);
    let code = err.get("code").and_then(|c| c.as_str());
    let hint = match code {
        Some("tool_use_failed") => {
            if msg.contains("enhanced_grep") || raw_msg.contains("enhanced_grep") {
                " For `enhanced_grep`, send `pattern` plus `file_paths` and/or `file_path` and/or `path`. For huge payloads, use a narrower scope or paste results as text."
            } else if msg.contains("edit_file") || raw_msg.contains("edit_file") {
                " For `edit_file`, send `path` or `file_path`, and the before/after text via `original_snippet`/`new_snippet` and/or `old_string`/`new_string` (you can mix these names). If the payload was huge, split into smaller edits."
            } else if (msg.contains("create_file") || raw_msg.contains("create_file"))
                && (msg.contains("file_path") || raw_msg.contains("file_path"))
            {
                " For `create_file`, use `file_path` or `path` plus `content`. If the payload was huge, split into smaller writes."
            } else {
                " Try a smaller tool call, split the change into steps, or ask the model to output the patch as text instead of one huge tool call."
            }
        }
        _ => "",
    };
    format!("{}{}", msg, hint)
}

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
    /// Normal chunks include `choices`; API error payloads omit it (handled before deserializing here).
    #[serde(default)]
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

                                    // Parse as Value first: error payloads are valid JSON but do not match `StreamChunk`
                                    // (no `choices`), which previously produced noisy "missing field `choices`" warnings.
                                    let v: Value = match serde_json::from_str(data_trim) {
                                        Ok(v) => v,
                                        Err(e) => {
                                            if data_trim.contains("tool_use_failed")
                                                || data_trim.contains("\"error\"")
                                            {
                                                yield Err(anyhow::Error::msg(
                                                    "Invalid JSON in API stream (tool/API error payload may be truncated).",
                                                ));
                                                return;
                                            }
                                            eprintln!(
                                                "Parse warning: invalid JSON in SSE ({}): {}",
                                                e,
                                                data_trim.chars().take(200).collect::<String>()
                                            );
                                            continue;
                                        }
                                    };

                                    if v.get("error").is_some() {
                                        let msg = stream_api_error_user_message(&v);
                                        yield Err(anyhow::Error::msg(msg));
                                        return;
                                    }

                                    let chunk: StreamChunk = match serde_json::from_value(v) {
                                        Ok(c) => c,
                                        Err(e) => {
                                            eprintln!(
                                                "Parse warning: unexpected SSE shape ({}): {}",
                                                e,
                                                data_trim.chars().take(200).collect::<String>()
                                            );
                                            continue;
                                        }
                                    };

                                    yield Ok(chunk);
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
