#![allow(dead_code)]

use anyhow::{Context, Result};
use futures::StreamExt;
use reqwest::header::HeaderMap;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, VecDeque};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Shorten repetitive Groq tool-schema validation messages for the terminal.
fn simplify_tool_validation_message(msg: &str) -> String {
    if !msg.contains("did not match schema")
        && !msg.contains("tool call validation")
        && !msg.contains("failed_generation")
        && !msg.contains("Failed to call a function")
    {
        return msg.to_string();
    }
    // Groq sometimes echoes the same phrase twice and lists every failed schema branch.
    let one_line: String = msg.split_whitespace().collect::<Vec<_>>().join(" ");
    if one_line.len() <= 280 {
        return one_line;
    }
    if one_line.contains("failed_generation") || one_line.contains("Failed to call a function") {
        return "Failed to call a function. See 'failed_generation' for more details.".to_string();
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
        Some("failed_generation") => {
            " Retry with smaller tool calls: inspect first, split edits into smaller `edit_file`/`create_file` calls, or describe a large patch as text instead of emitting one huge function call."
        }
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

/// Per-request generation settings (model behavior, not transport).
#[derive(Debug, Clone)]
pub struct RequestTuning {
    pub max_completion_tokens: u32,
    pub temperature: f32,
    /// `low`/`medium`/`high` for GPT-OSS models, `none`/`default` for Qwen3.
    /// Only sent when the active model supports the configured value.
    pub reasoning_effort: Option<String>,
    /// Groq service tier (`auto`, `on_demand`, `flex`, `performance`). Groq-only.
    pub service_tier: Option<String>,
}

impl Default for RequestTuning {
    fn default() -> Self {
        Self {
            max_completion_tokens: 4_096,
            temperature: 0.3,
            reasoning_effort: None,
            service_tier: None,
        }
    }
}

/// Authoritative rate-limit info from Groq `x-ratelimit-*` response headers.
#[derive(Debug, Clone, Copy)]
struct RateHeaderSnapshot {
    remaining_tokens: u64,
    reset_after: Duration,
    captured_at: Instant,
}

/// How long a header snapshot stays authoritative before we fall back to local estimates.
const RATE_HEADER_FRESHNESS: Duration = Duration::from_secs(30);

/// Sliding window of (request instant, estimated tokens) per model.
type RateWindows = HashMap<String, VecDeque<(Instant, u32)>>;

/// Groq OpenAI-compatible Chat Completions client (`https://api.groq.com/openai/v1`).
#[derive(Debug, Clone)]
pub struct GroqClient {
    client: Client,
    api_key: String,
    base_url: String,
    model: String,
    tuning: RequestTuning,
    tpm_limit: u32,
    route_wait_threshold: Duration,
    /// Local sliding-window token estimates per model (fallback when headers are stale).
    rate_window: Arc<Mutex<RateWindows>>,
    /// Latest `x-ratelimit-*` header snapshot per model (authoritative when fresh).
    rate_headers: Arc<Mutex<HashMap<String, RateHeaderSnapshot>>>,
}

/// Chat completion request (OpenAI-compatible subset). Borrows messages/tools so
/// building a request never clones the conversation history.
#[derive(Debug, Serialize)]
pub struct ChatRequest<'a> {
    pub model: &'a str,
    pub messages: &'a [Message],
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<&'a [Tool]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<&'a str>,
    pub stream: bool,
    /// Groq deprecated `max_tokens` in favor of `max_completion_tokens`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_completion_tokens: Option<u32>,
    /// Sent for non-Groq OpenAI-compatible servers (e.g. LM Studio).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    pub temperature: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service_tier: Option<&'a str>,
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
    /// Groq attaches final usage to the last streamed chunk under `x_groq`.
    #[serde(default)]
    pub x_groq: Option<XGroq>,
}

impl StreamChunk {
    /// Usage from either the OpenAI-style top-level field or Groq's `x_groq` envelope.
    pub fn effective_usage(&self) -> Option<&Usage> {
        self.usage
            .as_ref()
            .or_else(|| self.x_groq.as_ref().and_then(|x| x.usage.as_ref()))
    }
}

/// Groq-specific envelope on the final stream chunk.
#[derive(Debug, Deserialize)]
pub struct XGroq {
    pub usage: Option<Usage>,
}

/// Usage statistics
#[derive(Debug, Deserialize, Clone, Copy, Default)]
pub struct Usage {
    pub prompt_tokens: Option<u32>,
    pub completion_tokens: Option<u32>,
    pub total_tokens: Option<u32>,
    #[serde(default)]
    pub prompt_tokens_details: Option<PromptTokensDetails>,
}

impl Usage {
    /// Prompt tokens served from Groq's prefix cache (50% cheaper, exempt from TPM limits).
    pub fn cached_tokens(&self) -> u32 {
        self.prompt_tokens_details
            .and_then(|d| d.cached_tokens)
            .unwrap_or(0)
    }

    /// Tokens that actually count against TPM limits (cached prefix tokens are free).
    pub fn billable_tokens(&self) -> u32 {
        let total = self.total_tokens.unwrap_or_else(|| {
            self.prompt_tokens.unwrap_or(0) + self.completion_tokens.unwrap_or(0)
        });
        total.saturating_sub(self.cached_tokens())
    }
}

#[derive(Debug, Deserialize, Clone, Copy, Default)]
pub struct PromptTokensDetails {
    pub cached_tokens: Option<u32>,
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
    pub fn new(api_key: String, base_url: String, model: String, tuning: RequestTuning) -> Self {
        Self {
            client: Client::builder()
                .timeout(std::time::Duration::from_secs(300))
                .build()
                .expect("Failed to create HTTP client"),
            api_key,
            base_url,
            model,
            tuning,
            tpm_limit: std::env::var("VYBRID_GROQ_TPM_LIMIT")
                .ok()
                .and_then(|s| s.trim().parse::<u32>().ok())
                .filter(|n| *n > 0)
                .unwrap_or(240_000),
            route_wait_threshold: Duration::from_secs(
                std::env::var("VYBRID_ROUTE_WAIT_THRESHOLD_SECS")
                    .ok()
                    .and_then(|s| s.trim().parse::<u64>().ok())
                    .unwrap_or(5),
            ),
            rate_window: Arc::new(Mutex::new(HashMap::new())),
            rate_headers: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn with_model(&self, model: impl Into<String>) -> Self {
        Self {
            client: self.client.clone(),
            api_key: self.api_key.clone(),
            base_url: self.base_url.clone(),
            model: model.into(),
            tuning: self.tuning.clone(),
            tpm_limit: self.tpm_limit,
            route_wait_threshold: self.route_wait_threshold,
            rate_window: self.rate_window.clone(),
            rate_headers: self.rate_headers.clone(),
        }
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    fn is_groq(&self) -> bool {
        self.base_url.contains("groq.com")
    }

    fn is_openrouter(&self) -> bool {
        self.base_url.contains("openrouter.ai")
    }

    fn build_request<'a>(
        &'a self,
        messages: &'a [Message],
        tools: Option<&'a [Tool]>,
        stream: bool,
    ) -> ChatRequest<'a> {
        let has_tools = tools.is_some_and(|tools| !tools.is_empty());
        let is_groq = self.is_groq();
        ChatRequest {
            model: &self.model,
            messages,
            tools: tools.filter(|t| !t.is_empty()),
            tool_choice: has_tools.then_some("auto"),
            stream,
            max_completion_tokens: is_groq.then_some(self.tuning.max_completion_tokens),
            max_tokens: (!is_groq).then_some(self.tuning.max_completion_tokens),
            temperature: self.tuning.temperature,
            reasoning_effort: reasoning_effort_for_model(
                &self.model,
                self.tuning.reasoning_effort.as_deref(),
            ),
            service_tier: if is_groq {
                self.tuning.service_tier.as_deref()
            } else {
                None
            },
        }
    }

    /// Store the latest `x-ratelimit-*` header snapshot for this model.
    fn record_rate_headers(&self, headers: &HeaderMap) {
        if !self.is_groq() {
            return;
        }
        let remaining = headers
            .get("x-ratelimit-remaining-tokens")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.trim().parse::<f64>().ok())
            .map(|n| n.max(0.0) as u64);
        let reset = headers
            .get("x-ratelimit-reset-tokens")
            .and_then(|v| v.to_str().ok())
            .and_then(parse_reset_duration);
        if let (Some(remaining_tokens), Some(reset_after)) = (remaining, reset) {
            let mut map = self.rate_headers.lock().unwrap();
            map.insert(
                self.model.clone(),
                RateHeaderSnapshot {
                    remaining_tokens,
                    reset_after,
                    captured_at: Instant::now(),
                },
            );
        }
    }

    fn fresh_header_snapshot(&self) -> Option<RateHeaderSnapshot> {
        let map = self.rate_headers.lock().unwrap();
        map.get(&self.model)
            .copied()
            .filter(|snap| snap.captured_at.elapsed() < RATE_HEADER_FRESHNESS)
    }

    /// Replace the latest local window estimate with the actual billable usage the
    /// API reported. Cached prefix tokens are exempt from TPM limits, so estimates
    /// based on request size dramatically over-count once the cache is warm.
    fn reconcile_actual_usage(&self, usage: &Usage) {
        if !self.is_groq() {
            return;
        }
        let billable = usage.billable_tokens();
        let mut windows = self.rate_window.lock().unwrap();
        if let Some(window) = windows.get_mut(&self.model) {
            if let Some(back) = window.back_mut() {
                back.1 = billable;
            }
        }
    }

    async fn throttle_if_needed(&self, requested: u32) -> Result<()> {
        if !self.is_groq() {
            return Ok(());
        }

        // Prefer the server's own rate-limit headers when fresh: they are exact and
        // already account for cached-prefix tokens, unlike the local estimate window.
        if let Some(snap) = self.fresh_header_snapshot() {
            if snap.remaining_tokens >= requested as u64 {
                self.push_window_estimate(requested);
                return Ok(());
            }
            let wait = snap
                .reset_after
                .saturating_sub(snap.captured_at.elapsed())
                .min(Duration::from_secs(60));
            if wait.is_zero() {
                self.push_window_estimate(requested);
                return Ok(());
            }
            if wait > self.route_wait_threshold {
                anyhow::bail!(
                    "preflight_route_required: model `{}` reports {} remaining TPM tokens for an estimated {} token request (reset in {}s)",
                    self.model,
                    snap.remaining_tokens,
                    requested,
                    wait.as_secs().max(1)
                );
            }
            eprintln!(
                "Groq TPM preflight: waiting {}s for token window reset before sending estimated {} token request",
                wait.as_secs().max(1),
                requested
            );
            tokio::time::sleep(wait).await;
            self.push_window_estimate(requested);
            return Ok(());
        }

        // Fallback: local sliding-window estimates.
        let now = Instant::now();
        let wait = {
            let mut windows = self.rate_window.lock().unwrap();
            let window = windows.entry(self.model.clone()).or_default();
            while let Some((instant, _)) = window.front() {
                if now.duration_since(*instant) > Duration::from_secs(60) {
                    window.pop_front();
                } else {
                    break;
                }
            }
            let used: u32 = window.iter().map(|(_, tokens)| *tokens).sum();
            if used.saturating_add(requested) > self.tpm_limit {
                window.front().map(|(oldest, _)| {
                    (
                        Duration::from_secs(60).saturating_sub(now.duration_since(*oldest)),
                        used,
                    )
                })
            } else {
                window.push_back((now, requested));
                None
            }
        };

        if let Some((wait, used)) = wait {
            if !wait.is_zero() {
                if wait > self.route_wait_threshold {
                    anyhow::bail!(
                        "preflight_route_required: model `{}` would wait {}s before estimated {} token request (used {}, limit {})",
                        self.model,
                        wait.as_secs().max(1),
                        requested,
                        used,
                        self.tpm_limit
                    );
                }
                eprintln!(
                    "Groq TPM preflight: waiting {}s before sending estimated {} token request",
                    wait.as_secs().max(1),
                    requested
                );
                tokio::time::sleep(wait).await;
            }
            self.push_window_estimate(requested);
        }
        Ok(())
    }

    fn push_window_estimate(&self, requested: u32) {
        let mut windows = self.rate_window.lock().unwrap();
        let window = windows.entry(self.model.clone()).or_default();
        let now = Instant::now();
        while let Some((instant, _)) = window.front() {
            if now.duration_since(*instant) > Duration::from_secs(60) {
                window.pop_front();
            } else {
                break;
            }
        }
        window.push_back((now, requested));
    }

    async fn send_request(&self, body: String, accept_sse: bool) -> Result<reqwest::Response> {
        let mut builder = self
            .client
            .post(format!("{}/chat/completions", self.base_url))
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json");
        if self.is_openrouter() {
            builder = builder
                .header("HTTP-Referer", "https://github.com/SampleBias/vybrid")
                .header("X-OpenRouter-Title", "Vybrid");
        }
        if accept_sse {
            builder = builder.header("Accept", "text/event-stream");
        }
        let response = builder
            .body(body)
            .send()
            .await
            .context("Failed to send request to Groq API")?;

        self.record_rate_headers(response.headers());

        if !response.status().is_success() {
            let status = response.status();
            let retry_after = response
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.trim().parse::<f64>().ok());
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "Unknown error".to_string());
            anyhow::bail!(
                "API error ({}): {}",
                status,
                enrich_api_error_body(status.as_u16(), &body, retry_after)
            );
        }
        Ok(response)
    }

    /// Stream chat completion response
    pub async fn chat_stream(
        &self,
        messages: &[Message],
        tools: Option<&[Tool]>,
    ) -> Result<Pin<Box<dyn futures::Stream<Item = Result<StreamChunk>> + Send>>> {
        let request = self.build_request(messages, tools, true);
        let body = serde_json::to_string(&request).context("Failed to serialize chat request")?;
        let estimated_tokens = (body.len() / 4) as u32 + self.tuning.max_completion_tokens;

        self.throttle_if_needed(estimated_tokens).await?;

        let response = self.send_request(body, true).await?;

        let stream = response.bytes_stream();
        let usage_client = self.clone();

        let output_stream = async_stream::stream! {
            let mut buffer = String::new();

            futures::pin_mut!(stream);

            while let Some(chunk_result) = stream.next().await {
                match chunk_result {
                    Ok(bytes) => {
                        buffer.push_str(&String::from_utf8_lossy(&bytes));

                        while let Some(pos) = buffer.find("\n\n") {
                            // Drain shifts the remainder in place instead of reallocating
                            // a new String per SSE event.
                            let event: String = buffer.drain(..pos + 2).collect();

                            for line in event.lines() {
                                if let Some(data) = line.strip_prefix("data: ") {
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

                                    if let Some(usage) = chunk.effective_usage() {
                                        usage_client.reconcile_actual_usage(usage);
                                    }

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
    pub async fn chat(&self, messages: &[Message], tools: Option<&[Tool]>) -> Result<Message> {
        let request = self.build_request(messages, tools, false);
        let body = serde_json::to_string(&request).context("Failed to serialize chat request")?;
        let estimated_tokens = (body.len() / 4) as u32 + self.tuning.max_completion_tokens;

        self.throttle_if_needed(estimated_tokens).await?;

        let response = self.send_request(body, false).await?;

        #[derive(Deserialize)]
        struct ChatResponse {
            choices: Vec<ChatChoice>,
            usage: Option<Usage>,
        }

        #[derive(Deserialize)]
        struct ChatChoice {
            message: Message,
        }

        let chat_response: ChatResponse = response
            .json()
            .await
            .context("Failed to parse API response")?;

        if let Some(usage) = &chat_response.usage {
            self.reconcile_actual_usage(usage);
        }

        chat_response
            .choices
            .into_iter()
            .next()
            .map(|c| c.message)
            .context("No response from API")
    }
}

/// Gate `reasoning_effort` on per-model support so we never send a value the
/// provider would reject (`low`/`medium`/`high` → GPT-OSS, `none`/`default` → Qwen3).
fn reasoning_effort_for_model<'a>(model: &str, configured: Option<&'a str>) -> Option<&'a str> {
    let effort = configured?.trim();
    if effort.is_empty() {
        return None;
    }
    let gpt_oss = model.contains("gpt-oss");
    let qwen3 = model.contains("qwen3");
    match effort {
        "low" | "medium" | "high" if gpt_oss => Some(effort),
        "none" | "default" if qwen3 => Some(effort),
        _ => None,
    }
}

/// Parse Groq reset header durations like `7.66s`, `2m59.56s`, `1h2m`, `454ms`.
fn parse_reset_duration(s: &str) -> Option<Duration> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let mut total_secs = 0f64;
    let mut num = String::new();
    let mut unit = String::new();

    let flush = |num: &mut String, unit: &mut String, total: &mut f64| -> bool {
        if num.is_empty() {
            return unit.is_empty();
        }
        let Ok(value) = num.parse::<f64>() else {
            return false;
        };
        let mult = match unit.as_str() {
            "h" => 3600.0,
            "m" => 60.0,
            "s" | "" => 1.0,
            "ms" => 0.001,
            _ => return false,
        };
        *total += value * mult;
        num.clear();
        unit.clear();
        true
    };

    for ch in s.chars() {
        if ch.is_ascii_digit() || ch == '.' {
            if !unit.is_empty() && !flush(&mut num, &mut unit, &mut total_secs) {
                return None;
            }
            num.push(ch);
        } else if ch.is_ascii_alphabetic() {
            unit.push(ch);
        } else {
            return None;
        }
    }
    if !flush(&mut num, &mut unit, &mut total_secs) {
        return None;
    }
    Some(Duration::from_secs_f64(total_secs.max(0.0)))
}

fn enrich_api_error_body(status: u16, body: &str, retry_after_header: Option<f64>) -> String {
    if status != 429 {
        return body.to_string();
    }
    // Make sure a machine-readable retry hint is always present: the agent loop
    // parses "Please try again in <secs>s" to schedule its retry.
    let retry_hint = if !body.contains("Please try again in") {
        retry_after_header
            .map(|secs| format!(" Please try again in {secs}s."))
            .unwrap_or_default()
    } else {
        String::new()
    };
    let limit = number_after(body, "Limit ");
    let used = number_after(body, "Used ");
    let requested = number_after(body, "Requested ");
    let retry = number_after(body, "Please try again in ")
        .or_else(|| retry_after_header.map(|s| s.to_string()));
    match (limit, used, requested) {
        (Some(limit), Some(used), Some(requested)) => format!(
            "{body}{retry_hint}\nGroq TPM details: limit={limit}, used={used}, requested={requested}, retry_after_seconds={}. Lower VYBRID_GROQ_CONTEXT_TOKEN_BUDGET or refresh/use index.md to reduce request size.",
            retry.unwrap_or_default()
        ),
        _ => format!(
            "{body}{retry_hint}\nGroq rate limit hit. Lower VYBRID_GROQ_CONTEXT_TOKEN_BUDGET or use index.md/root-relative targeted reads to reduce request size."
        ),
    }
}

fn number_after(body: &str, marker: &str) -> Option<String> {
    let start = body.find(marker)? + marker.len();
    let suffix = &body[start..];
    let end = suffix
        .char_indices()
        .find_map(|(idx, ch)| (!ch.is_ascii_digit() && ch != '.').then_some(idx))
        .unwrap_or(suffix.len());
    let value = suffix[..end].trim();
    (!value.is_empty()).then(|| value.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enriches_groq_tpm_errors_with_budget_hint() {
        let body = "Rate limit reached for model `openai/gpt-oss-120b` on tokens per minute (TPM): Limit 250000, Used 224739, Requested 88828. Please try again in 15.25s.";
        let enriched = enrich_api_error_body(429, body, None);

        assert!(enriched.contains("limit=250000"));
        assert!(enriched.contains("used=224739"));
        assert!(enriched.contains("requested=88828"));
        assert!(enriched.contains("VYBRID_GROQ_CONTEXT_TOKEN_BUDGET"));
    }

    #[test]
    fn adds_retry_hint_from_header_when_body_lacks_one() {
        let body = r#"{"error":{"message":"Rate limit reached","code":"rate_limit_exceeded"}}"#;
        let enriched = enrich_api_error_body(429, body, Some(12.0));

        assert!(enriched.contains("Please try again in 12s."));
    }

    #[test]
    fn parses_reset_durations() {
        assert_eq!(
            parse_reset_duration("7.66s"),
            Some(Duration::from_secs_f64(7.66))
        );
        assert_eq!(
            parse_reset_duration("2m59.56s"),
            Some(Duration::from_secs_f64(179.56))
        );
        assert_eq!(
            parse_reset_duration("1h2m3s"),
            Some(Duration::from_secs_f64(3723.0))
        );
        assert_eq!(
            parse_reset_duration("454ms"),
            Some(Duration::from_secs_f64(0.454))
        );
        assert_eq!(parse_reset_duration(""), None);
        assert_eq!(parse_reset_duration("soon"), None);
    }

    #[test]
    fn gates_reasoning_effort_by_model() {
        assert_eq!(
            reasoning_effort_for_model("openai/gpt-oss-120b", Some("low")),
            Some("low")
        );
        assert_eq!(
            reasoning_effort_for_model("openai/gpt-oss-120b", Some("none")),
            None
        );
        assert_eq!(
            reasoning_effort_for_model("qwen/qwen3-32b", Some("none")),
            Some("none")
        );
        assert_eq!(
            reasoning_effort_for_model("qwen/qwen3-32b", Some("low")),
            None
        );
        assert_eq!(
            reasoning_effort_for_model("groq/compound", Some("low")),
            None
        );
        assert_eq!(
            reasoning_effort_for_model("openai/gpt-oss-120b", None),
            None
        );
    }

    #[test]
    fn usage_billable_tokens_exclude_cached_prefix() {
        let usage = Usage {
            prompt_tokens: Some(10_000),
            completion_tokens: Some(500),
            total_tokens: Some(10_500),
            prompt_tokens_details: Some(PromptTokensDetails {
                cached_tokens: Some(8_000),
            }),
        };

        assert_eq!(usage.cached_tokens(), 8_000);
        assert_eq!(usage.billable_tokens(), 2_500);
    }

    #[test]
    fn groq_requests_use_max_completion_tokens() {
        let client = GroqClient::new(
            "key".to_string(),
            "https://api.groq.com/openai/v1".to_string(),
            "openai/gpt-oss-120b".to_string(),
            RequestTuning {
                max_completion_tokens: 1024,
                temperature: 0.3,
                reasoning_effort: Some("low".to_string()),
                service_tier: Some("auto".to_string()),
            },
        );
        let messages = vec![Message {
            role: "user".to_string(),
            content: Some("hello".to_string()),
            tool_calls: None,
            tool_call_id: None,
        }];
        let request = client.build_request(&messages, None, true);
        let json = serde_json::to_value(&request).unwrap();

        assert_eq!(json["max_completion_tokens"], 1024);
        assert!(json.get("max_tokens").is_none());
        assert_eq!(json["reasoning_effort"], "low");
        assert_eq!(json["service_tier"], "auto");
        assert!(json.get("tools").is_none());
        assert!(json.get("tool_choice").is_none());
    }

    #[test]
    fn non_groq_requests_use_max_tokens_and_skip_groq_params() {
        let client = GroqClient::new(
            "lm-studio".to_string(),
            "http://127.0.0.1:1234/v1".to_string(),
            "local-model".to_string(),
            RequestTuning {
                max_completion_tokens: 1024,
                temperature: 0.3,
                reasoning_effort: Some("low".to_string()),
                service_tier: Some("auto".to_string()),
            },
        );
        let messages = vec![Message {
            role: "user".to_string(),
            content: Some("hello".to_string()),
            tool_calls: None,
            tool_call_id: None,
        }];
        let request = client.build_request(&messages, None, false);
        let json = serde_json::to_value(&request).unwrap();

        assert_eq!(json["max_tokens"], 1024);
        assert!(json.get("max_completion_tokens").is_none());
        assert!(json.get("reasoning_effort").is_none());
        assert!(json.get("service_tier").is_none());
    }
}
