mod client;
mod config;
mod conversation;
mod lsp;
mod project_docs;
mod rust_agent_reference;
mod shell;
mod tools;
mod ui;

use anyhow::{Context, Result};
use console::style;
use dialoguer::{Confirm, Input, Select};
use futures::StreamExt;
use std::io::{self, Write};

use crate::client::groq::{GroqClient, Message, ToolCall};
use crate::config::{Config, LlmProvider};
use crate::conversation::{Conversation, REQUEST_CONTEXT_TOKEN_BUDGET};
use crate::lsp::RustLspManager;
use crate::project_docs::ProjectDocs;
use crate::tools::definitions::get_all_tools;
use crate::tools::executor::{execute_tool_with_context, ToolRuntime};

#[tokio::main]
async fn main() -> Result<()> {
    let mut args = std::env::args();
    let _exe = args.next();
    if let Some(flag) = args.next() {
        if flag == "--version" || flag == "-V" {
            println!("vybrid {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
    }

    let config = match Config::load() {
        Ok(c) => c,
        Err(e) => {
            ui::print_error(&format!("Configuration error: {}", e));
            eprintln!("\nCould not initialize config directories or environment.");
            std::process::exit(1);
        }
    };

    // Display banner
    ui::display_banner();

    // Run agent mode directly
    run_agent_mode(config).await
}

/// Run agent mode - interactive AI assistant
async fn run_agent_mode(mut config: Config) -> Result<()> {
    ui::display_mode_header();
    ui::display_cwd();

    let mut client = rebuild_llm_client(&config);
    let rust_lsp = RustLspManager::new(
        config.rust_lsp_command.clone(),
        config.rust_lsp_root.clone(),
    );

    if config.rust_lsp_enabled {
        let root = resolve_rust_lsp_root(&config)?;
        if let Err(e) = rust_lsp.connect(&config.rust_lsp_command, root).await {
            ui::print_error(&format!("Rust LSP auto-connect failed: {}", e));
        }
    }

    if client.is_none() {
        let tip = match config.llm_provider {
            LlmProvider::Groq => "No Groq API key found. After you add keys once, they are saved to ~/.vybrid/.env and vybrid-rust/.env (kept in sync) so Vybrid works from any directory.",
            LlmProvider::LmStudio => "LM Studio is selected but chat is not configured (set LM_STUDIO_MODEL to your loaded model id, start the local server, and use /menu). Keys are saved to ~/.vybrid/.env and vybrid-rust/.env.",
        };
        println!("{}", style(tip).dim());
        println!();
        if let Ok(true) = Confirm::new()
            .with_prompt("Open setup menu to add API keys now?")
            .default(true)
            .interact()
        {
            if let Err(e) = handle_menu(&mut config, &mut client, &rust_lsp).await {
                ui::print_error(&format!("{}", e));
            }
        }
        if client.is_none() {
            println!(
                "{}",
                style(format!(
                    "Tip: run /menu when ready — keys are written to:\n  {}\n  {}",
                    config.global_env_file_path.display(),
                    config.env_file_path.display()
                ))
                .dim()
            );
            println!();
        }
    }

    let project_docs = ProjectDocs::new();
    let mut conversation = Conversation::new(&get_system_prompt());
    let tool_runtime = ToolRuntime {
        rust_lsp: Some(rust_lsp.clone()),
    };

    loop {
        let rust_lsp_status = rust_lsp.status().await;
        ui::print_context_status_line(conversation.estimate_context_tokens(), &rust_lsp_status);
        print!("{} ", style("You>").magenta().bold());
        io::stdout().flush()?;

        let mut input = String::new();
        match io::stdin().read_line(&mut input) {
            Ok(0) => break, // EOF
            Ok(_) => {}
            Err(e) => {
                ui::print_error(&format!("Input error: {}", e));
                continue;
            }
        }

        let input = input.trim();

        if input.is_empty() {
            continue;
        }

        // Handle special commands
        match input.to_lowercase().as_str() {
            "exit" | "quit" => {
                println!("{}", style("Goodbye!").cyan());
                break;
            }
            "clear" => {
                ui::clear_screen();
                ui::display_banner();
                ui::display_mode_header();
                ui::display_cwd();
                continue;
            }
            "/pwd" | "pwd" => {
                ui::display_cwd();
                continue;
            }
            "/tools" => {
                show_available_tools();
                continue;
            }
            "/help" => {
                show_help();
                continue;
            }
            "/menu" => {
                if let Err(e) = handle_menu(&mut config, &mut client, &rust_lsp).await {
                    ui::print_error(&format!("{}", e));
                }
                continue;
            }
            "/new" => {
                conversation.clear_keeping_system();
                println!("{}", style("Started new conversation").green());
                continue;
            }
            "!" => {
                if let Err(e) = shell::enter_shell_mode() {
                    ui::print_error(&format!("Shell mode error: {}", e));
                }
                ui::display_cwd();
                continue;
            }
            _ => {}
        }

        // Handle /docs command
        if input.starts_with("/docs") {
            handle_docs_command(input, &project_docs);
            continue;
        }

        // Handle single shell commands (!command)
        if let Some(cmd) = input.strip_prefix('!') {
            match tools::shell::execute_bash(cmd, None, None).await {
                Ok(output) => println!("{}", output),
                Err(e) => ui::print_error(&format!("Command failed: {}", e)),
            }
            continue;
        }

        // Handle /add command
        if let Some(path) = input.strip_prefix("/add ") {
            match tools::file_ops::read_file(path) {
                Ok(content) => {
                    conversation.add_user_message(&format!(
                        "I'm adding this file to our conversation:\n\n{}",
                        content
                    ));
                    println!(
                        "{}",
                        style(format!("Added '{}' to conversation", path)).green()
                    );
                }
                Err(e) => ui::print_error(&format!("Failed to read '{}': {}", path, e)),
            }
            continue;
        }

        let Some(ref c) = client else {
            let msg = match config.llm_provider {
                LlmProvider::Groq => format!(
                    "No Groq API key. Use /menu — keys are saved to {} and {}.",
                    config.global_env_file_path.display(),
                    config.env_file_path.display()
                ),
                LlmProvider::LmStudio => format!(
                    "LM Studio is not ready (model id, server, or API token). Use /menu — settings: {} and {}.",
                    config.global_env_file_path.display(),
                    config.env_file_path.display()
                ),
            };
            ui::print_error(&msg);
            continue;
        };

        // Add user message to conversation with project docs context
        let user_message_with_context = inject_project_docs(input, &project_docs);
        conversation.add_user_message(&user_message_with_context);

        let tools = get_all_tools();

        // Process with AI
        let spinner_label = llm_spinner_label(config.llm_provider);
        let mut rate_limit_fallback = RateLimitFallbackState::new(
            matches!(config.llm_provider, LlmProvider::Groq),
            config.groq_rate_limit_fallback_model.clone(),
        );
        if let Err(e) = process_ai_response(
            c,
            &mut conversation,
            &tools,
            &tool_runtime,
            0,
            spinner_label,
            &mut rate_limit_fallback,
        )
        .await
        {
            ui::print_error(&format!("AI error: {}", e));
        }

        println!();
    }

    Ok(())
}

fn llm_spinner_label(provider: LlmProvider) -> &'static str {
    match provider {
        LlmProvider::Groq => "groq",
        LlmProvider::LmStudio => "local",
    }
}

struct RateLimitFallbackState {
    enabled: bool,
    waits: u32,
    fallback_model: String,
    using_fallback: bool,
}

impl RateLimitFallbackState {
    fn new(enabled: bool, fallback_model: String) -> Self {
        Self {
            enabled,
            waits: 0,
            fallback_model,
            using_fallback: false,
        }
    }

    fn client_for_request(&self, primary: &GroqClient) -> GroqClient {
        if self.enabled && self.using_fallback {
            primary.with_model(self.fallback_model.clone())
        } else {
            primary.clone()
        }
    }

    fn record_rate_limit_wait(&mut self) -> bool {
        self.waits += 1;
        if self.enabled && !self.using_fallback && self.waits >= 2 {
            self.using_fallback = true;
            true
        } else {
            false
        }
    }

    fn fallback_model(&self) -> &str {
        &self.fallback_model
    }
}

/// The API may reject a tool call before streaming any assistant text; these errors are worth one automatic retry.
fn is_retryable_groq_stream_error(e: &anyhow::Error) -> bool {
    let s = e.to_string();
    let s_lower = s.to_ascii_lowercase();
    s_lower.contains("tool_use_failed")
        || s_lower.contains("did not match schema")
        || s_lower.contains("tool call validation")
        || s_lower.contains("failed to parse tool call arguments as json")
        || s_lower.contains("invalid json in api stream")
        || is_failed_generation_error(e)
}

fn is_failed_generation_error(e: &anyhow::Error) -> bool {
    let s = e.to_string().to_ascii_lowercase();
    s.contains("failed_generation")
        || s.contains("failed to call a function")
        || s.contains("please adjust your prompt")
}

fn is_tool_argument_json_error(e: &anyhow::Error) -> bool {
    let s = e.to_string();
    s.contains("Failed to parse tool call arguments as JSON")
        || s.contains("Invalid JSON in API stream")
        || s.contains("tool/API error payload may be truncated")
}

fn is_context_length_error(e: &anyhow::Error) -> bool {
    let s = e.to_string();
    s.contains("context_length_exceeded")
        || s.contains("Please reduce the length of the messages or completion")
}

fn is_rate_limit_error(e: &anyhow::Error) -> bool {
    let s = e.to_string();
    s.contains("429 Too Many Requests")
        || s.contains("rate_limit_exceeded")
        || s.contains("Rate limit reached")
}

fn parse_retry_after_seconds(message: &str) -> Option<f64> {
    let marker = "Please try again in ";
    let start = message.find(marker)? + marker.len();
    let suffix = &message[start..];
    let end = suffix
        .char_indices()
        .find_map(|(idx, ch)| (!ch.is_ascii_digit() && ch != '.').then_some(idx))
        .unwrap_or(suffix.len());
    suffix[..end].parse::<f64>().ok()
}

fn rate_limit_retry_delay(e: &anyhow::Error) -> std::time::Duration {
    let seconds = parse_retry_after_seconds(&e.to_string()).unwrap_or(15.0);
    let seconds = seconds.ceil().clamp(1.0, 60.0) + 1.0;
    std::time::Duration::from_secs(seconds as u64)
}

fn rate_limit_resume_prompt(attempt: u32, max_attempts: u32) -> String {
    format!(
        "[Vybrid] The previous API request hit the provider token-per-minute rate limit. This was attempt {attempt} of {max_attempts}. Continue the current task from the latest messages and tool results. Keep the next request concise, avoid re-reading context unless needed, and prefer small tool calls."
    )
}

#[cfg(test)]
mod retry_tests {
    use super::*;

    #[test]
    fn parses_groq_retry_after_seconds() {
        let message = r#"API error (429 Too Many Requests): {"error":{"message":"Rate limit reached for model `openai/gpt-oss-120b` on tokens per minute (TPM): Limit 250000, Used 224739, Requested 88828. Please try again in 15.256079999s. ","type":"tokens","code":"rate_limit_exceeded"}}"#;

        assert_eq!(parse_retry_after_seconds(message), Some(15.256079999));
    }

    #[test]
    fn recognizes_rate_limit_errors() {
        let err = anyhow::anyhow!("API error (429 Too Many Requests): rate_limit_exceeded");

        assert!(is_rate_limit_error(&err));
    }

    #[test]
    fn recognizes_failed_generation_errors() {
        let err = anyhow::anyhow!(
            "Failed to call a function. Please adjust your prompt. See 'failed_generation' for more details."
        );

        assert!(is_failed_generation_error(&err));
        assert!(is_retryable_groq_stream_error(&err));
    }

    #[test]
    fn failed_generation_prompt_prefers_smaller_tool_calls() {
        let err = anyhow::anyhow!(
            "Failed to call a function. Please adjust your prompt. See 'failed_generation' for more details."
        );
        let prompt = corrective_tool_prompt(&err, 2, 5);

        assert!(prompt.contains("failed to generate a valid tool call"));
        assert!(prompt.contains("read_file"));
        assert!(prompt.contains("edit_file"));
        assert!(prompt.contains("create_multiple_files"));
    }
}

fn corrective_tool_prompt(e: &anyhow::Error, attempt: u32, max_attempts: u32) -> String {
    if is_failed_generation_error(e) {
        format!(
            "[Vybrid] The provider failed to generate a valid tool call: {e}. This was attempt {attempt} of {max_attempts}. Retry the task now using smaller, simpler tool calls. First inspect with read_file, read_multiple_files, enhanced_grep, cargo_metadata, or rust_project_snapshot as needed. For edits, prefer edit_file with a small exact snippet. Avoid create_multiple_files for large content; split file creation into separate small create_file calls. If the patch is large or multiline-heavy, explain the patch as text instead of calling a tool."
        )
    } else if is_tool_argument_json_error(e) {
        format!(
            "[Vybrid] The API rejected your tool call because its arguments were not valid JSON: {e}. This was attempt {attempt} of {max_attempts}. Retry the task now. If you need to edit or create large/multiline content, split it into smaller tool calls. Do not place raw unescaped multiline text in tool arguments; escape JSON strings correctly, or explain the patch as text instead of calling a tool."
        )
    } else {
        format!(
            "[Vybrid] The API rejected a tool call before the reply could complete: {e}. This was attempt {attempt} of {max_attempts}. Please respond again; use tools only with arguments that match each tool's schema (see descriptions). For enhanced_grep, include `pattern` plus `path`, `file_path`, or `file_paths`."
        )
    }
}

/// Process AI response with streaming and tool calls
async fn process_ai_response(
    client: &GroqClient,
    conversation: &mut Conversation,
    tools: &[crate::client::groq::Tool],
    tool_runtime: &ToolRuntime,
    depth: u32,
    spinner_label: &'static str,
    rate_limit_fallback: &mut RateLimitFallbackState,
) -> Result<()> {
    /// Prevents runaway tool loops when the model chains many rounds.
    const MAX_TOOL_ROUNDS: u32 = 48;
    if depth >= MAX_TOOL_ROUNDS {
        anyhow::bail!(
            "Stopped: tool loop exceeded {} rounds. Try /new or a smaller task.",
            MAX_TOOL_ROUNDS
        );
    }

    /// If the API rejects a tool call, retry with a corrective user note. Some providers stream
    /// reasoning before reporting malformed tool JSON, so retry after thinking but before content.
    const MAX_STREAM_ATTEMPTS: u32 = 5;

    let mut reasoning_started: bool;
    let mut content_started: bool;
    let mut final_content = String::new();
    let mut tool_calls: Vec<ToolCall> = Vec::new();
    let mut first_chunk: bool;
    let mut rate_limit_note_added = false;

    let mut attempt: u32 = 0;
    'stream: loop {
        attempt += 1;

        reasoning_started = false;
        content_started = false;
        final_content.clear();
        tool_calls.clear();
        first_chunk = true;

        let request_budget = match attempt {
            1 => REQUEST_CONTEXT_TOKEN_BUDGET,
            2 => 40_000,
            _ => 18_000,
        };
        let request_messages = if attempt == 1 {
            conversation.messages_for_request()
        } else {
            conversation.messages_for_request_with_budget(request_budget)
        };

        let mut spinner = ui::SpinnerGuard::new(spinner_label);
        let request_client = rate_limit_fallback.client_for_request(client);
        let stream = request_client
            .chat_stream(request_messages, Some(tools.to_vec()))
            .await;
        let stream = match stream {
            Ok(s) => s,
            Err(e) => {
                spinner.finish().await;
                if attempt < MAX_STREAM_ATTEMPTS && is_rate_limit_error(&e) {
                    let delay = rate_limit_retry_delay(&e);
                    let switching_to_fallback = rate_limit_fallback.record_rate_limit_wait();
                    if !rate_limit_note_added {
                        conversation.add_user_message(&rate_limit_resume_prompt(
                            attempt,
                            MAX_STREAM_ATTEMPTS,
                        ));
                        rate_limit_note_added = true;
                    }
                    println!(
                        "{}",
                        style(format!(
                            "Rate limit reached — waiting {}s, then retrying with a compacted {}k-token request...",
                            delay.as_secs(),
                            match attempt {
                                1 => 40,
                                _ => 18,
                            }
                        ))
                        .yellow()
                    );
                    if switching_to_fallback {
                        println!(
                            "{}",
                            style(format!(
                                "Primary Groq model is still rate limited; using {} for this response.",
                                rate_limit_fallback.fallback_model()
                            ))
                            .yellow()
                        );
                    }
                    let mut wait_spinner = ui::SpinnerGuard::new("rate limit");
                    tokio::time::sleep(delay).await;
                    wait_spinner.finish().await;
                    continue 'stream;
                }
                if attempt < MAX_STREAM_ATTEMPTS && is_context_length_error(&e) {
                    println!(
                        "{}",
                        style(format!(
                            "Context was too large for the provider — retrying with a compacted {}k-token request...",
                            match attempt {
                                1 => 40,
                                _ => 18,
                            }
                        ))
                        .yellow()
                    );
                    continue 'stream;
                }
                return Err(e);
            }
        };

        futures::pin_mut!(stream);

        while let Some(chunk_result) = stream.next().await {
            if first_chunk {
                spinner.finish().await;
                first_chunk = false;
            }
            match chunk_result {
                Ok(chunk) => {
                    if let Some(choice) = chunk.choices.first() {
                        // Handle reasoning content (thinking)
                        if let Some(reasoning) = &choice.delta.reasoning_content {
                            if !reasoning_started {
                                println!();
                                println!("{}", style("Thinking:").blue().dim());
                                reasoning_started = true;
                            }
                            print!("{}", style(reasoning).dim());
                            io::stdout().flush()?;
                        }

                        // Handle content
                        if let Some(content) = &choice.delta.content {
                            if !content_started {
                                if reasoning_started {
                                    println!();
                                    println!();
                                }
                                print!("{} ", style("Assistant>").cyan().bold());
                                content_started = true;
                            }
                            print!("{}", content);
                            io::stdout().flush()?;
                            final_content.push_str(content);
                        }

                        // Handle tool calls
                        if let Some(tc_deltas) = &choice.delta.tool_calls {
                            for tc_delta in tc_deltas {
                                while tool_calls.len() <= tc_delta.index {
                                    tool_calls.push(ToolCall {
                                        id: String::new(),
                                        call_type: "function".to_string(),
                                        function: crate::client::groq::FunctionCall {
                                            name: String::new(),
                                            arguments: String::new(),
                                        },
                                    });
                                }

                                if let Some(id) = &tc_delta.id {
                                    tool_calls[tc_delta.index].id.push_str(id);
                                }
                                if let Some(func) = &tc_delta.function {
                                    if let Some(name) = &func.name {
                                        tool_calls[tc_delta.index].function.name.push_str(name);
                                    }
                                    if let Some(args) = &func.arguments {
                                        tool_calls[tc_delta.index]
                                            .function
                                            .arguments
                                            .push_str(args);
                                    }
                                }
                            }
                        }
                    }
                }
                Err(e) => {
                    println!();
                    if attempt < MAX_STREAM_ATTEMPTS && is_rate_limit_error(&e) && !content_started
                    {
                        spinner.finish().await;
                        let delay = rate_limit_retry_delay(&e);
                        let switching_to_fallback = rate_limit_fallback.record_rate_limit_wait();
                        if !rate_limit_note_added {
                            conversation.add_user_message(&rate_limit_resume_prompt(
                                attempt,
                                MAX_STREAM_ATTEMPTS,
                            ));
                            rate_limit_note_added = true;
                        }
                        println!(
                            "{}",
                            style(format!(
                                "Rate limit reached mid-stream — waiting {}s, then retrying compacted...",
                                delay.as_secs()
                            ))
                            .yellow()
                        );
                        if switching_to_fallback {
                            println!(
                                "{}",
                                style(format!(
                                    "Primary Groq model is still rate limited; using {} for this response.",
                                    rate_limit_fallback.fallback_model()
                                ))
                                .yellow()
                            );
                        }
                        let mut wait_spinner = ui::SpinnerGuard::new("rate limit");
                        tokio::time::sleep(delay).await;
                        wait_spinner.finish().await;
                        continue 'stream;
                    }
                    if attempt < MAX_STREAM_ATTEMPTS
                        && is_retryable_groq_stream_error(&e)
                        && !content_started
                    {
                        conversation.add_user_message(&corrective_tool_prompt(
                            &e,
                            attempt,
                            MAX_STREAM_ATTEMPTS,
                        ));
                        println!(
                            "{}",
                            style(
                                "Tool call validation failed — retrying with a corrective prompt..."
                            )
                            .yellow()
                        );
                        spinner.finish().await;
                        continue 'stream;
                    }
                    spinner.finish().await;
                    return Err(e);
                }
            }
        }

        if first_chunk {
            spinner.finish().await;
        }

        break 'stream;
    }

    if content_started || reasoning_started {
        println!();
    }

    // Store assistant message
    let assistant_msg = Message {
        role: "assistant".to_string(),
        content: if final_content.is_empty() {
            None
        } else {
            Some(final_content)
        },
        tool_calls: if tool_calls.is_empty() {
            None
        } else {
            Some(tool_calls.clone())
        },
        tool_call_id: None,
    };
    conversation.add_assistant_message(assistant_msg);

    // Execute tool calls if any
    if !tool_calls.is_empty() {
        ui::print_tool_execution(tool_calls.len());

        for tool_call in &tool_calls {
            if tool_call.function.name.is_empty() {
                continue;
            }

            ui::print_tool_call(&tool_call.function.name);

            let result = execute_tool_with_context(
                &tool_call.function.name,
                &tool_call.function.arguments,
                tool_runtime,
            )
            .await;

            match &result {
                Ok(output) => {
                    ui::print_tool_result(&tool_call.function.name, true);
                    // Show brief output for some tools
                    if tool_call.function.name == "execute_bash_command"
                        || tool_call.function.name == "run_cargo"
                    {
                        let preview: String = output.lines().take(5).collect::<Vec<_>>().join("\n");
                        if !preview.is_empty() {
                            println!("    {}", style(preview).dim());
                        }
                    }
                }
                Err(e) => {
                    ui::print_tool_result(&tool_call.function.name, false);
                    println!("    {}", style(e.to_string()).red().dim());
                }
            }

            // Add tool result to conversation
            let result_str = result.unwrap_or_else(|e| format!("Error: {}", e));
            conversation.add_tool_result(&tool_call.id, &result_str);
        }

        // Get follow-up response (next round shows its own spinner)
        println!();
        Box::pin(process_ai_response(
            client,
            conversation,
            tools,
            tool_runtime,
            depth + 1,
            spinner_label,
            rate_limit_fallback,
        ))
        .await?;
    }

    Ok(())
}

/// Inject project documentation context into user message
fn inject_project_docs(user_message: &str, project_docs: &ProjectDocs) -> String {
    const MAX_PROJECT_DOC_CHARS: usize = 24_000;
    match project_docs.read() {
        Ok(Some(docs)) => {
            let docs = if docs.chars().count() > MAX_PROJECT_DOC_CHARS {
                let excerpt: String = docs.chars().take(MAX_PROJECT_DOC_CHARS).collect();
                format!(
                    "{excerpt}\n\n[Project docs truncated: showing first {MAX_PROJECT_DOC_CHARS} chars. Add narrower docs or relevant files when needed.]"
                )
            } else {
                docs
            };
            format!("{}\n\n---\n\nPROJECT CONTEXT:\n{}", user_message, docs)
        }
        Ok(None) | Err(_) => user_message.to_string(),
    }
}

/// Get the system prompt for agent mode
fn get_system_prompt() -> String {
    format!(
        r#"You are Vybrid, an elite Rust coding agent. Your job is to solve software engineering tasks with compiler-aware Rust judgment, careful file inspection, and verified edits.

RUST OPERATING POLICY:
1. For unfamiliar Rust projects, inspect crate shape first with `rust_project_snapshot`, `cargo_metadata`, `Cargo.toml`, and nearby modules.
2. Understand ownership, borrowing, lifetimes, trait bounds, enums, async constraints, and error boundaries before editing.
3. Prefer small, reversible edits that address the root cause; avoid speculative rewrites.
4. Use `run_cargo` for Rust build/test/lint loops. Use `diagnostic_format: "json"` when compiler diagnostics are important or output is noisy.
5. Fix primary compiler errors before warnings; rustc ordering and spans are authoritative.
6. After changes, verify with `cargo check`, then `cargo test` or `cargo clippy` when appropriate.
7. Explain Rust trade-offs at the user's requested depth; be concrete about ownership, trait, enum, lifetime, and async reasoning.
8. Only create task/docs scaffolding when the user asks for project scaffolding or the target repository already follows that workflow.

RUST CARGO QUICK REFERENCE:
{}

COMPILE / FIX LOOP:
{}

COMMON RUST DIAGNOSTICS:
{}

RUST REVIEW HEURISTICS:
{}

Available tools:
- read_file, read_multiple_files: Read file contents
- create_file, create_multiple_files: Create or overwrite files
- edit_file: Make precise edits using snippet replacement
- run_cargo: Run Cargo (check, build, test, clippy, fmt, doc, …) with structured argv — preferred for Rust projects over raw shell when invoking cargo
- rust_project_snapshot, cargo_metadata: Inspect Rust workspace/package layout before editing
- explain_rust_diagnostic: Explain rustc error codes and Rust topics such as ownership, traits, enums, lifetimes, and async Send
- rust_lsp_query: Use connected rust-analyzer LSP for status, diagnostics, hover, definition, references, symbols, completions, code actions, and formatting edits. Always pass an `operation` string, for example {{"operation":"status"}} or {{"operation":"diagnostics","file_path":"src/main.rs"}}
- execute_bash_command: Run shell commands (rustup, system packages, non-cargo scripts)
- enhanced_grep: Search files with regex patterns
- google_search: Search for information online
- create_project_structure: Initialize project files
- get_current_todo_items: List incomplete tasks
- mark_todo_complete: Mark a task as done

TOOL CALL SAFETY:
1. Every tool call argument payload must be valid JSON. Escape quotes, backslashes, and newlines inside strings.
2. Keep edit/create tool payloads small. For large files, large patches, or many replacements, split the change into several tool calls or describe the patch as text first.
3. Prefer read_file/enhanced_grep before editing, then use the smallest exact snippet replacement that proves the intended change.
4. If a tool call is rejected for invalid JSON or schema mismatch, retry with simpler arguments instead of repeating the same payload.

Guidelines:
1. Read project files before editing to understand context
2. Use precise snippet matching for edits
3. On Rust projects, use run_cargo for compile/test/lint iteration; read compiler diagnostics before fixing
4. Explain what you're doing and why
5. Be thorough in analysis and recommendations

IMPORTANT: Be efficient and thorough. If the user asks for implementation, proceed with the smallest verified changes that satisfy the request."#,
        crate::rust_agent_reference::RUST_CARGO_QUICKREF,
        crate::rust_agent_reference::RUST_COMPILE_FIX_LOOP,
        crate::rust_agent_reference::RUST_DIAGNOSTICS_HINTS,
        crate::rust_agent_reference::RUST_REVIEW_HEURISTICS,
    )
}

/// Show available tools
fn show_available_tools() {
    println!();
    println!("{}", style("Available Tools:").cyan().bold());
    println!("{}", style("─".repeat(40)).dim());

    let tools = get_all_tools();
    for tool in tools {
        // Truncate description for display
        let desc: String = tool
            .function
            .description
            .lines()
            .next()
            .unwrap_or("")
            .to_string();
        println!(
            "  {} - {}",
            style(&tool.function.name).yellow(),
            style(&desc).dim()
        );
    }

    println!();
}

/// Show help
fn show_help() {
    println!();
    println!("{}", style("Vybrid Commands:").cyan().bold());
    println!("{}", style("─".repeat(40)).dim());
    println!("  {}  - Exit Vybrid", style("exit, quit").yellow());
    println!(
        "  {}       - Enter persistent shell mode",
        style("!").yellow()
    );
    println!(
        "  {}    - Execute single shell command",
        style("!<cmd>").yellow()
    );
    println!(
        "  {} - Add file to conversation",
        style("/add <path>").yellow()
    );
    println!(
        "  {}       - Show current directory",
        style("/pwd").yellow()
    );
    println!(
        "  {}     - List available AI tools",
        style("/tools").yellow()
    );
    println!(
        "  {}       - Start new conversation",
        style("/new").yellow()
    );
    println!("  {}     - Clear screen", style("clear").yellow());
    println!("  {}      - Show this help", style("/help").yellow());
    println!(
        "  {}     - Menu (Groq / LM Studio / SerpAPI / Rust LSP)",
        style("/menu").yellow()
    );
    println!();
    println!("{}", style("Project Docs Commands:").cyan().bold());
    println!("{}", style("─".repeat(40)).dim());
    println!(
        "  {}           - Show current project docs",
        style("/docs").yellow()
    );
    println!(
        "  {}  - Add docs from a file",
        style("/docs add <file>").yellow()
    );
    println!(
        "  {}          - Read docs interactively",
        style("/docs read").yellow()
    );
    println!(
        "  {}         - Clear project docs",
        style("/docs clear").yellow()
    );
    println!();
}

/// Handle /docs commands
fn handle_docs_command(input: &str, project_docs: &ProjectDocs) {
    let parts: Vec<&str> = input.splitn(3, ' ').collect();

    match parts.get(1).map(|s| s.to_lowercase()).as_deref() {
        None | Some("show") | Some("") => {
            // Show current docs
            match project_docs.read() {
                Ok(Some(content)) => {
                    println!();
                    println!("{}", style("Current Project Documentation:").cyan().bold());
                    println!("{}", style("─".repeat(40)).dim());
                    println!("{}", content);
                    println!();
                }
                Ok(None) => {
                    println!("{}", style("No project documentation found.").dim());
                    println!("Create one with: {}", style("/docs read").yellow());
                    println!("Or add from file: {}", style("/docs add <file>").yellow());
                }
                Err(e) => {
                    ui::print_error(&format!("Failed to read project docs: {}", e));
                }
            }
        }
        Some("add") => {
            // Add docs from a file
            if let Some(path) = parts.get(2) {
                match tools::file_ops::read_file(path) {
                    Ok(content) => match project_docs.add(&content) {
                        Ok(_) => {
                            println!(
                                "{}",
                                style(format!(
                                    "Added documentation from '{}' to project docs",
                                    path
                                ))
                                .green()
                            );
                        }
                        Err(e) => {
                            ui::print_error(&format!("Failed to add docs: {}", e));
                        }
                    },
                    Err(e) => {
                        ui::print_error(&format!("Failed to read '{}': {}", path, e));
                    }
                }
            } else {
                println!("{}", style("Usage: /docs add <file>").dim());
            }
        }
        Some("read") => {
            // Read docs interactively
            println!();
            println!(
                "{}",
                style("Enter project documentation (empty line to finish):")
                    .cyan()
                    .bold()
            );
            println!("{}", style("─".repeat(40)).dim());

            let mut lines = Vec::new();
            loop {
                print!("> ");
                io::stdout().flush().unwrap();

                let mut line = String::new();
                match io::stdin().read_line(&mut line) {
                    Ok(0) => break,
                    Ok(_) => {
                        if line.trim().is_empty() {
                            break;
                        }
                        lines.push(line.trim_end().to_string());
                    }
                    Err(e) => {
                        ui::print_error(&format!("Input error: {}", e));
                        break;
                    }
                }
            }

            if lines.is_empty() {
                println!("{}", style("No documentation entered.").dim());
                return;
            }

            let content = lines.join("\n");
            match project_docs.add(&content) {
                Ok(_) => {
                    println!("{}", style("Project documentation saved.").green());
                }
                Err(e) => {
                    ui::print_error(&format!("Failed to save docs: {}", e));
                }
            }
        }
        Some("clear") => {
            // Clear docs
            match project_docs.clear() {
                Ok(_) => {
                    println!("{}", style("Project documentation cleared.").green());
                }
                Err(e) => {
                    ui::print_error(&format!("Failed to clear docs: {}", e));
                }
            }
        }
        Some(unknown) => {
            println!(
                "{}",
                style(format!("Unknown /docs subcommand: '{}'", unknown)).red()
            );
            println!("Available subcommands: show, add, read, clear");
        }
    }
}

fn saved_env_locations(config: &Config) -> String {
    format!(
        "{}\n  {}",
        config.global_env_file_path.display(),
        config.env_file_path.display()
    )
}

/// Build the OpenAI-compatible chat client for the active LLM provider.
fn rebuild_llm_client(config: &Config) -> Option<GroqClient> {
    config
        .effective_chat_client_params()
        .map(|(api_key, base_url, model)| GroqClient::new(api_key, base_url, model))
}

fn resolve_rust_lsp_root(config: &Config) -> Result<std::path::PathBuf> {
    let root = config
        .rust_lsp_root
        .clone()
        .unwrap_or(std::env::current_dir().context("Could not resolve current directory")?);
    if root.is_absolute() {
        Ok(root)
    } else {
        Ok(std::env::current_dir()
            .context("Could not resolve current directory")?
            .join(root))
    }
}

/// Interactive menu — keys written to `~/.vybrid/.env` and `vybrid-rust/.env`
async fn handle_menu(
    config: &mut Config,
    client: &mut Option<GroqClient>,
    rust_lsp: &RustLspManager,
) -> Result<()> {
    let items = vec![
        "Add Groq + optional SerpAPI keys (then save & use chat)",
        "Add or update Groq API key only",
        "Configure LM Studio (local server — OpenAI-compatible)",
        "Add or update SerpAPI key only (Google search)",
        "Switch to Groq (cloud)",
        "Rust LSP (rust-analyzer)",
        "Back",
    ];
    let sel = Select::new()
        .with_prompt("Vybrid menu")
        .items(&items)
        .default(0)
        .interact()
        .context("Menu cancelled")?;

    match sel {
        0 => {
            let key: String = Input::new()
                .with_prompt("Groq API key")
                .interact_text()
                .context("No API key entered")?;
            let key = key.trim().to_string();
            if key.is_empty() {
                ui::print_error("Groq API key was empty.");
                return Ok(());
            }
            config.set_groq_api_key(key)?;
            *client = rebuild_llm_client(config);

            let serp: String = Input::new()
                .with_prompt("SerpAPI key (optional — Enter to skip)")
                .allow_empty(true)
                .interact_text()
                .context("SerpAPI prompt failed")?;
            let serp = serp.trim();
            if !serp.is_empty() {
                config.set_serpapi_key(serp.to_string())?;
            }

            println!(
                "{}",
                style(format!(
                    "Saved key(s) — you can start chatting. Files updated:\n  {}",
                    saved_env_locations(config)
                ))
                .green()
            );
        }
        1 => {
            let key: String = Input::new()
                .with_prompt("Groq API key")
                .interact_text()
                .context("No API key entered")?;
            let key = key.trim().to_string();
            if key.is_empty() {
                ui::print_error("API key was empty.");
                return Ok(());
            }
            config.set_groq_api_key(key)?;
            *client = rebuild_llm_client(config);
            println!(
                "{}",
                style(format!(
                    "Saved GROQ_API_KEY to:\n  {}",
                    saved_env_locations(config)
                ))
                .green()
            );
        }
        2 => {
            let default_base = crate::config::DEFAULT_LM_STUDIO_BASE_URL;
            let base_raw: String = Input::new()
                .with_prompt(format!(
                    "LM Studio OpenAI base URL (Enter for {})",
                    default_base
                ))
                .allow_empty(true)
                .interact_text()
                .context("Base URL prompt failed")?;
            let base = if base_raw.trim().is_empty() {
                default_base.to_string()
            } else {
                base_raw.trim().to_string()
            };
            let api_key: String = Input::new()
                .with_prompt("LM Studio API key (empty = placeholder when auth is off)")
                .allow_empty(true)
                .interact_text()
                .context("API key prompt failed")?;
            let model: String = Input::new()
                .with_prompt("Model id (must match the model loaded in LM Studio)")
                .interact_text()
                .context("Model id required")?;
            let model = model.trim().to_string();
            if model.is_empty() {
                ui::print_error("Model id was empty.");
                return Ok(());
            }
            config.apply_lm_studio_profile(base, api_key, model)?;
            *client = rebuild_llm_client(config);
            println!(
                "{}",
                style(format!(
                    "Saved LM Studio profile (VYBRID_LLM_PROVIDER=lmstudio) to:\n  {}",
                    saved_env_locations(config)
                ))
                .green()
            );
        }
        3 => {
            let key: String = Input::new()
                .with_prompt("SerpAPI key")
                .interact_text()
                .context("No API key entered")?;
            let key = key.trim().to_string();
            if key.is_empty() {
                ui::print_error("SerpAPI key was empty.");
                return Ok(());
            }
            config.set_serpapi_key(key)?;
            println!(
                "{}",
                style(format!(
                    "Saved SERPAPI_KEY to:\n  {}",
                    saved_env_locations(config)
                ))
                .green()
            );
        }
        4 => {
            config.set_llm_provider(LlmProvider::Groq)?;
            *client = rebuild_llm_client(config);
            if client.is_some() {
                println!(
                    "{}",
                    style(format!(
                        "Switched to Groq. VYBRID_LLM_PROVIDER=groq — settings:\n  {}",
                        saved_env_locations(config)
                    ))
                    .green()
                );
            } else {
                ui::print_error(
                    "VYBRID_LLM_PROVIDER is now groq, but GROQ_API_KEY is missing. Use \"Add or update Groq API key only\".",
                );
            }
        }
        5 => {
            handle_rust_lsp_menu(config, rust_lsp).await?;
        }
        _ => {}
    }
    Ok(())
}

async fn handle_rust_lsp_menu(config: &mut Config, rust_lsp: &RustLspManager) -> Result<()> {
    loop {
        let status = rust_lsp.status().await;
        let items = vec![
            "Connect now",
            "Disconnect",
            "Restart",
            "Show status",
            if config.rust_lsp_enabled {
                "Disable auto-connect"
            } else {
                "Enable auto-connect"
            },
            "Configure rust-analyzer command",
            "Configure workspace root",
            "Back",
        ];
        let sel = Select::new()
            .with_prompt(format!(
                "Rust LSP menu ({})",
                status.summary().lines().next().unwrap_or("Rust LSP")
            ))
            .items(&items)
            .default(0)
            .interact()
            .context("Rust LSP menu cancelled")?;

        match sel {
            0 => {
                let root = resolve_rust_lsp_root(config)?;
                rust_lsp.connect(&config.rust_lsp_command, root).await?;
                println!("{}", style("Rust LSP connected.").green());
            }
            1 => {
                rust_lsp.disconnect().await?;
                println!("{}", style("Rust LSP disconnected.").green());
            }
            2 => {
                let root = resolve_rust_lsp_root(config)?;
                rust_lsp.restart(&config.rust_lsp_command, root).await?;
                println!("{}", style("Rust LSP restarted.").green());
            }
            3 => {
                println!("{}", style(rust_lsp.status().await.summary()).dim());
            }
            4 => {
                let enabled = !config.rust_lsp_enabled;
                config.set_rust_lsp_enabled(enabled)?;
                if enabled {
                    let root = resolve_rust_lsp_root(config)?;
                    match rust_lsp.connect(&config.rust_lsp_command, root).await {
                        Ok(()) => println!("{}", style("Rust LSP auto-connect enabled.").green()),
                        Err(e) => ui::print_error(&format!(
                            "Auto-connect enabled, but connection failed: {}",
                            e
                        )),
                    }
                } else {
                    rust_lsp.disconnect().await?;
                    println!("{}", style("Rust LSP auto-connect disabled.").green());
                }
            }
            5 => {
                let command: String = Input::new()
                    .with_prompt("Rust LSP command")
                    .default(config.rust_lsp_command.clone())
                    .interact_text()
                    .context("Rust LSP command prompt failed")?;
                config.set_rust_lsp_command(command)?;
                println!(
                    "{}",
                    style(format!(
                        "Saved VYBRID_RUST_LSP_COMMAND to:\n  {}",
                        saved_env_locations(config)
                    ))
                    .green()
                );
            }
            6 => {
                let current = config
                    .rust_lsp_root
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default();
                let root: String = Input::new()
                    .with_prompt("Workspace root (empty = current directory at runtime)")
                    .default(current)
                    .allow_empty(true)
                    .interact_text()
                    .context("Rust LSP root prompt failed")?;
                let root = root.trim();
                if root.is_empty() {
                    config.set_rust_lsp_root(None)?;
                } else {
                    config.set_rust_lsp_root(Some(std::path::PathBuf::from(root)))?;
                }
                println!(
                    "{}",
                    style(format!(
                        "Saved VYBRID_RUST_LSP_ROOT to:\n  {}",
                        saved_env_locations(config)
                    ))
                    .green()
                );
            }
            _ => break,
        }
    }
    Ok(())
}
