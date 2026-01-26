#![allow(dead_code)]

use anyhow::Result;
use console::style;
use futures::StreamExt;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Instant;
use uuid::Uuid;

use crate::client::glm::{GlmClient, Message, ToolCall};
use crate::config::Config;
use crate::tools::{definitions::get_all_tools, executor::execute_tool};

use super::queue::{ExecutionRequest, ExecutionResponse, MessageQueue};

/// Daemon worker that processes execution requests
pub struct Worker {
    id: usize,
    client: GlmClient,
    queue: Arc<MessageQueue>,
    session_id: String,
    running: Arc<AtomicBool>,
}

impl Worker {
    pub fn new(
        id: usize,
        config: &Config,
        queue: Arc<MessageQueue>,
        running: Arc<AtomicBool>,
    ) -> Self {
        let client = GlmClient::new(
            config.api_key.clone(),
            config.api_base_url.clone(),
            config.model.clone(),
        );

        Self {
            id,
            client,
            queue,
            session_id: Uuid::new_v4().to_string(),
            running,
        }
    }

    /// Process a single request
    pub async fn process_request(&self, request: ExecutionRequest) -> Result<ExecutionResponse> {
        let start_time = Instant::now();

        eprintln!(
            "{} Worker {} processing request {}",
            style("→").cyan(),
            self.id,
            &request.id[..8]
        );
        eprintln!("  Query: {}", request.user_query);
        eprintln!("  Dir: {}", request.current_directory);

        // Update progress
        self.queue.update_progress(&request.id, "started", Some(0.0), Some("Request received"))?;

        // Check if cancelled
        if self.queue.is_cancelled(&request.id) {
            return Ok(ExecutionResponse::cancelled(&request.id, &self.session_id));
        }

        // Validate directory
        if !std::path::Path::new(&request.current_directory).exists() {
            return Ok(ExecutionResponse::error(
                &request.id,
                format!("Directory does not exist: {}", request.current_directory),
                &self.session_id,
            ));
        }

        // Change to request directory
        let original_dir = std::env::current_dir()?;
        std::env::set_current_dir(&request.current_directory)?;

        self.queue.update_progress(&request.id, "processing", Some(0.2), Some("Processing AI request"))?;

        // Process the request
        let result = self.execute_ai_request(&request).await;

        // Restore original directory
        std::env::set_current_dir(&original_dir)?;

        let processing_time = start_time.elapsed().as_secs_f64();

        self.queue.update_progress(&request.id, "completed", Some(1.0), Some("Request completed"))?;

        match result {
            Ok(output) => {
                eprintln!(
                    "{} Worker {} completed request {} in {:.2}s",
                    style("✓").green(),
                    self.id,
                    &request.id[..8],
                    processing_time
                );
                Ok(ExecutionResponse::success(
                    &request.id,
                    output,
                    &self.session_id,
                    processing_time,
                ))
            }
            Err(e) => {
                eprintln!(
                    "{} Worker {} failed request {}: {}",
                    style("✗").red(),
                    self.id,
                    &request.id[..8],
                    e
                );
                Ok(ExecutionResponse::error(
                    &request.id,
                    e.to_string(),
                    &self.session_id,
                ))
            }
        }
    }

    async fn execute_ai_request(&self, request: &ExecutionRequest) -> Result<String> {
        // Daemon workers don't have delegation tools (false) to prevent circular dependencies
        let tools = get_all_tools(false);
        let system_prompt = get_daemon_system_prompt();

        let mut messages = vec![
            Message {
                role: "system".to_string(),
                content: Some(system_prompt),
                tool_calls: None,
                tool_call_id: None,
            },
            Message {
                role: "user".to_string(),
                content: Some(request.user_query.clone()),
                tool_calls: None,
                tool_call_id: None,
            },
        ];

        let mut final_output = String::new();
        let mut iterations = 0;
        const MAX_ITERATIONS: usize = 10;

        while iterations < MAX_ITERATIONS {
            iterations += 1;

            // Check for cancellation
            if self.queue.is_cancelled(&request.id) {
                return Err(anyhow::anyhow!("Request cancelled"));
            }

            let stream = self.client.chat_stream(messages.clone(), Some(tools.clone())).await?;
            futures::pin_mut!(stream);

            let mut content = String::new();
            let mut tool_calls: Vec<ToolCall> = Vec::new();

            while let Some(chunk_result) = stream.next().await {
                if let Ok(chunk) = chunk_result {
                    if let Some(choice) = chunk.choices.first() {
                        if let Some(c) = &choice.delta.content {
                            content.push_str(c);
                        }

                        if let Some(tc_deltas) = &choice.delta.tool_calls {
                            for tc_delta in tc_deltas {
                                while tool_calls.len() <= tc_delta.index {
                                    tool_calls.push(ToolCall {
                                        id: String::new(),
                                        call_type: "function".to_string(),
                                        function: crate::client::glm::FunctionCall {
                                            name: String::new(),
                                            arguments: String::new(),
                                        },
                                    });
                                }

                                if let Some(id) = &tc_delta.id {
                                    tool_calls[tc_delta.index].id = id.clone();
                                }
                                if let Some(func) = &tc_delta.function {
                                    if let Some(name) = &func.name {
                                        tool_calls[tc_delta.index].function.name.push_str(name);
                                    }
                                    if let Some(args) = &func.arguments {
                                        tool_calls[tc_delta.index].function.arguments.push_str(args);
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // Store assistant message
            messages.push(Message {
                role: "assistant".to_string(),
                content: if content.is_empty() { None } else { Some(content.clone()) },
                tool_calls: if tool_calls.is_empty() { None } else { Some(tool_calls.clone()) },
                tool_call_id: None,
            });

            if !content.is_empty() {
                final_output.push_str(&content);
                final_output.push('\n');
            }

            // If no tool calls, we're done
            if tool_calls.is_empty() {
                break;
            }

            // Execute tool calls
            // Note: Daemon workers don't have delegation capability to avoid circular dependencies
            for tool_call in &tool_calls {
                eprintln!("  → Executing: {}", tool_call.function.name);

                let result = execute_tool(
                    &tool_call.function.name,
                    &tool_call.function.arguments,
                    None, // No config = no delegation tools available in daemon mode
                ).await?;

                messages.push(Message {
                    role: "tool".to_string(),
                    content: Some(result),
                    tool_calls: None,
                    tool_call_id: Some(tool_call.id.clone()),
                });
            }

            self.queue.update_progress(
                &request.id,
                "processing",
                Some(0.2 + (0.7 * iterations as f32 / MAX_ITERATIONS as f32)),
                Some(&format!("Processing iteration {}", iterations)),
            )?;
        }

        if final_output.is_empty() {
            final_output = "Request processed successfully.".to_string();
        }

        Ok(final_output)
    }
}

fn get_daemon_system_prompt() -> String {
    r#"You are Vybrid, an elite software engineer running in daemon mode.
You are processing requests from chat mode users who need code execution and file operations.

WORKFLOW REQUIREMENTS:
1. Execute the requested operations efficiently
2. Use function calls for file operations and commands
3. Provide clear status updates in your responses
4. Handle errors gracefully

Available tools:
- read_file, read_multiple_files: Read file contents
- create_file, create_multiple_files: Create files
- edit_file: Edit files with snippet replacement
- execute_bash_command: Run shell commands
- enhanced_grep: Search files
- google_search: Search for information
- create_project_structure: Initialize project files

Be efficient and thorough. Execute the request and report results clearly."#.to_string()
}
