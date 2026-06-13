mod client;
mod config;
mod conversation;
mod lsp;
mod memory;
mod menu;
mod project_context;
mod project_docs;
mod project_index;
mod rust_agent_reference;
mod shell;
mod tools;
mod ui;

use anyhow::Result;
use console::style;
use dialoguer::Confirm;
use futures::StreamExt;
use std::borrow::Cow;
use std::io::{self, Write};

use crate::client::groq::{GroqClient, Message, Tool, ToolCall, Usage};
use crate::config::{Config, LlmProvider};
use crate::conversation::Conversation;
use crate::lsp::RustLspManager;
use crate::memory::{AutoDreamOutcome, MemoryStore};
use crate::project_docs::ProjectDocs;
use crate::tools::definitions::get_all_tools;
use crate::tools::executor::{execute_tool_with_context, is_read_only_tool, ToolRuntime};

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

    let mut client = config.build_chat_client();
    let rust_lsp = RustLspManager::new(
        config.rust_lsp_command.clone(),
        config.rust_lsp_root.clone(),
    );

    if config.rust_lsp_enabled {
        let root = menu::resolve_rust_lsp_root(&config)?;
        if let Err(e) = rust_lsp.connect(&config.rust_lsp_command, root).await {
            ui::print_error(&format!("Rust LSP auto-connect failed: {}", e));
        }
    }

    if client.is_none() {
        let tip = match config.llm_provider {
            LlmProvider::Groq => "No Groq API key found. After you add keys once, they are saved to ~/.vybrid/.env and vybrid-rust/.env (kept in sync) so Vybrid works from any directory.",
            LlmProvider::LmStudio => "LM Studio is selected but chat is not configured (set LM_STUDIO_MODEL to your loaded model id, start the local server, and use /menu). Keys are saved to ~/.vybrid/.env and vybrid-rust/.env.",
            LlmProvider::OpenRouter => "OpenRouter is selected but chat is not configured (add OPENROUTER_API_KEY and pick a model via /menu). Keys are saved to ~/.vybrid/.env and vybrid-rust/.env.",
        };
        println!("{}", style(tip).dim());
        println!();
        if let Ok(true) = Confirm::new()
            .with_prompt("Open setup menu to add API keys now?")
            .default(true)
            .interact()
        {
            if let Err(e) = menu::handle_menu(&mut config, &mut client, &rust_lsp).await {
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
    let memory_store = MemoryStore::new(config.messages_dir.clone());
    let mut conversation = Conversation::new(&get_system_prompt());
    let tool_runtime = ToolRuntime {
        rust_lsp: Some(rust_lsp.clone()),
        memory: Some(memory_store.clone()),
        file_read_cache: Default::default(),
        output_store: tools::output::ToolOutputStore::new(config.progress_dir.clone()),
    };

    loop {
        let rust_lsp_status = rust_lsp.status().await;
        ui::print_context_status_line(
            conversation.estimate_context_tokens(),
            config.context_token_budget,
            config.max_completion_tokens,
            &config.active_model_id(),
            config.reasoning_effort.as_deref(),
            &rust_lsp_status,
        );
        print!("{} ", style("You>").magenta().bold());
        io::stdout().flush()?;

        // Read on a blocking thread so the Tokio runtime (spinner, LSP, Ctrl-C
        // handling) is never stalled while waiting on the user.
        let input = match read_stdin_line().await {
            Ok(Some(line)) => line,
            Ok(None) => break, // EOF
            Err(e) => {
                ui::print_error(&format!("Input error: {}", e));
                continue;
            }
        };

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
                if let Err(e) = menu::handle_menu(&mut config, &mut client, &rust_lsp).await {
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

        if input.starts_with("/thinking") {
            handle_thinking_command(input, &mut config, &mut client);
            continue;
        }

        if input.starts_with("/index") {
            handle_index_command(input);
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
                LlmProvider::OpenRouter => format!(
                    "OpenRouter is not ready (API key or model). Use /menu — settings: {} and {}.",
                    config.global_env_file_path.display(),
                    config.env_file_path.display()
                ),
            };
            ui::print_error(&msg);
            continue;
        };

        // Add user message with lightweight project context and the memory index only.
        let user_message_with_context = inject_project_context(input, &project_docs, &memory_store);
        conversation.add_user_message(&user_message_with_context);
        record_latest_memory_message(&memory_store, &conversation);

        // Process with AI
        let spinner_label = llm_spinner_label(config.llm_provider);
        let mut route_state = RouteState::new(
            matches!(config.llm_provider, LlmProvider::Groq),
            config.groq_rate_limit_fallback_model.clone(),
            config.groq_compound_model.clone(),
            config.groq_compound_mini_model.clone(),
            config.compound_enabled,
        );
        let mut turn_state = TurnState::new(&config);
        if let Err(e) = process_ai_response(
            c,
            &mut conversation,
            &tool_runtime,
            0,
            spinner_label,
            &mut route_state,
            &mut turn_state,
        )
        .await
        {
            ui::print_error(&format!("AI error: {}", e));
        }

        println!();
    }

    run_autodream_idle_consolidation(&memory_store);

    Ok(())
}

/// Blocking stdin read off the async runtime. `Ok(None)` signals EOF.
async fn read_stdin_line() -> io::Result<Option<String>> {
    tokio::task::spawn_blocking(|| {
        let mut line = String::new();
        match io::stdin().read_line(&mut line) {
            Ok(0) => Ok(None),
            Ok(_) => Ok(Some(line)),
            Err(e) => Err(e),
        }
    })
    .await
    .unwrap_or_else(|e| Err(io::Error::other(format!("stdin task failed: {e}"))))
}

fn llm_spinner_label(provider: LlmProvider) -> &'static str {
    match provider {
        LlmProvider::Groq => "groq",
        LlmProvider::LmStudio => "local",
        LlmProvider::OpenRouter => "openrouter",
    }
}

fn run_autodream_idle_consolidation(memory_store: &MemoryStore) {
    match memory_store.complete_session_and_autodream() {
        Ok(AutoDreamOutcome::Consolidated {
            sessions,
            transcript_matches,
            ..
        }) => {
            println!(
                "{}",
                style(format!(
                    "autoDream consolidated memory from {sessions} completed session(s), using {transcript_matches} transcript signal(s)."
                ))
                .dim()
            );
        }
        Ok(AutoDreamOutcome::Skipped(_)) => {}
        Err(e) => eprintln!("{}", style(format!("autoDream warning: {e}")).dim()),
    }
}

/// Per-turn agent state: round budget (extendable with user consent) and
/// repeated-tool-call tracking for loop detection.
struct TurnState {
    /// Tool rounds allowed before asking the user whether to continue.
    max_rounds: u32,
    context_token_budget: u32,
    retry_context_token_budget: u32,
    /// Counts of identical read-only tool calls since the last mutating tool ran.
    repeated_tool_calls: std::collections::HashMap<String, u32>,
}

impl TurnState {
    fn new(config: &Config) -> Self {
        Self {
            max_rounds: config.max_tool_rounds,
            context_token_budget: config.context_token_budget,
            retry_context_token_budget: config.retry_context_token_budget,
            repeated_tool_calls: std::collections::HashMap::new(),
        }
    }
}

/// Detect the model spinning on the same read-only call. Mutating tools clear the
/// map (project state changed, so re-running an identical read is legitimate again).
/// Returns a corrective note to append to the tool result from the third identical
/// execution onward.
fn record_tool_call_repetition(
    seen: &mut std::collections::HashMap<String, u32>,
    name: &str,
    arguments: &str,
) -> Option<String> {
    if !is_read_only_tool(name) {
        seen.clear();
        return None;
    }
    let key = format!("{name}\u{1}{arguments}");
    let count = seen.entry(key).and_modify(|c| *c += 1).or_insert(1);
    if *count >= 3 {
        Some(format!(
            "\n\n[Vybrid] This exact `{name}` call has now run {count} times this turn with no file changes in between, so its output cannot differ. Do not repeat it. Use the result above and take the next concrete step toward finishing the task; if you are blocked, say so and summarize instead of calling tools."
        ))
    } else {
        None
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RouteMode {
    Primary,
    Fallback,
    Compound,
    CompoundMini,
}

struct RouteState {
    enabled: bool,
    waits: u32,
    fallback_model: String,
    compound_model: String,
    compound_mini_model: String,
    compound_enabled: bool,
    mode: RouteMode,
}

impl RouteState {
    fn new(
        enabled: bool,
        fallback_model: String,
        compound_model: String,
        compound_mini_model: String,
        compound_enabled: bool,
    ) -> Self {
        Self {
            enabled,
            waits: 0,
            fallback_model,
            compound_model,
            compound_mini_model,
            compound_enabled,
            mode: RouteMode::Primary,
        }
    }

    fn client_for_request(&self, primary: &GroqClient) -> GroqClient {
        if !self.enabled {
            return primary.clone();
        }
        match self.mode {
            RouteMode::Primary => primary.clone(),
            RouteMode::Fallback => primary.with_model(self.fallback_model.clone()),
            RouteMode::Compound => primary.with_model(self.compound_model.clone()),
            RouteMode::CompoundMini => primary.with_model(self.compound_mini_model.clone()),
        }
    }

    /// The full tool set is sent on every tool-capable request. Tool definitions
    /// are part of Groq's cached prompt prefix, so a stable set across rounds is
    /// both cheaper and faster than per-round reduced profiles (which caused a
    /// full cache miss whenever the profile changed between rounds).
    fn tools_for_request(&self) -> Option<&'static [Tool]> {
        match self.mode {
            RouteMode::Compound | RouteMode::CompoundMini => None,
            _ => Some(get_all_tools()),
        }
    }

    fn record_rate_limit_wait(&mut self) -> bool {
        self.waits += 1;
        if !self.enabled {
            return false;
        }
        self.advance_route()
    }

    fn route_preflight_wait(&mut self) -> bool {
        if !self.enabled {
            return false;
        }
        self.advance_route()
    }

    fn advance_route(&mut self) -> bool {
        let next = match self.mode {
            RouteMode::Primary => RouteMode::Fallback,
            RouteMode::Fallback if self.compound_enabled => RouteMode::Compound,
            RouteMode::Compound if self.compound_enabled => RouteMode::CompoundMini,
            _ => return false,
        };
        self.mode = next;
        true
    }

    fn route_label(&self, primary: &GroqClient) -> String {
        match self.mode {
            RouteMode::Primary => primary.model().to_string(),
            RouteMode::Fallback => self.fallback_model.clone(),
            RouteMode::Compound => self.compound_model.clone(),
            RouteMode::CompoundMini => self.compound_mini_model.clone(),
        }
    }

    fn is_compound(&self) -> bool {
        matches!(self.mode, RouteMode::Compound | RouteMode::CompoundMini)
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

fn should_retry_tool_generation_error(e: &anyhow::Error, content_started: bool) -> bool {
    is_retryable_groq_stream_error(e) && (!content_started || is_failed_generation_error(e))
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

fn is_preflight_route_error(e: &anyhow::Error) -> bool {
    e.to_string().contains("preflight_route_required")
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

    #[test]
    fn retries_failed_generation_after_content_started() {
        let err = anyhow::anyhow!(
            "Failed to call a function. Please adjust your prompt. See 'failed_generation' for more details."
        );

        assert!(should_retry_tool_generation_error(&err, true));
    }

    #[test]
    fn does_not_retry_schema_error_after_content_started() {
        let err = anyhow::anyhow!(
            "Tool call validation failed: parameters for tool read_file did not match schema"
        );

        assert!(!should_retry_tool_generation_error(&err, true));
        assert!(should_retry_tool_generation_error(&err, false));
    }

    #[test]
    fn repeated_read_only_calls_get_a_nudge_on_third_run() {
        let mut seen = std::collections::HashMap::new();
        let args = r#"{"file_path":"src/main.rs"}"#;

        assert!(record_tool_call_repetition(&mut seen, "read_file", args).is_none());
        assert!(record_tool_call_repetition(&mut seen, "read_file", args).is_none());
        let nudge = record_tool_call_repetition(&mut seen, "read_file", args);

        assert!(nudge.is_some());
        assert!(nudge.unwrap().contains("Do not repeat"));
    }

    #[test]
    fn different_arguments_are_not_repetitions() {
        let mut seen = std::collections::HashMap::new();

        for i in 0..5 {
            let args = format!(r#"{{"file_path":"src/file{i}.rs"}}"#);
            assert!(record_tool_call_repetition(&mut seen, "read_file", &args).is_none());
        }
    }

    #[test]
    fn mutating_calls_reset_repetition_tracking() {
        let mut seen = std::collections::HashMap::new();
        let args = r#"{"subcommand":"check"}"#;

        assert!(record_tool_call_repetition(&mut seen, "run_cargo", args).is_none());
        assert!(record_tool_call_repetition(&mut seen, "run_cargo", args).is_none());
        // run_cargo is mutating, so the map is cleared every time and a third
        // identical invocation (a legitimate compile/fix loop) is never flagged.
        assert!(record_tool_call_repetition(&mut seen, "run_cargo", args).is_none());
        assert!(seen.is_empty());

        let read_args = r#"{"file_path":"src/main.rs"}"#;
        assert!(record_tool_call_repetition(&mut seen, "read_file", read_args).is_none());
        assert!(record_tool_call_repetition(&mut seen, "read_file", read_args).is_none());
        // A mutation between reads makes a re-read legitimate again.
        assert!(record_tool_call_repetition(&mut seen, "edit_file", "{}").is_none());
        assert!(record_tool_call_repetition(&mut seen, "read_file", read_args).is_none());
    }

    #[test]
    fn compound_messages_end_with_user_and_drop_tool_roles() {
        let mut conversation = Conversation::new("system");
        conversation.add_user_message("please inspect");
        conversation.add_tool_result("call-1", "tool output");

        let messages = compound_messages_for_request(&mut conversation, 8_000);

        assert_eq!(messages.last().unwrap().role, "user");
        assert!(!messages.iter().any(|m| m.role == "tool"));
    }

    #[test]
    fn project_context_injects_memory_index_without_topic_or_transcript_contents() {
        let root = std::env::temp_dir().join(format!(
            "vybrid-main-memory-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let memory_dir = root.join(".vybrid").join("memory");
        std::fs::create_dir_all(memory_dir.join("topics")).unwrap();
        std::fs::write(
            memory_dir.join("MEMORY.md"),
            "- routing => topics/routing.md\n",
        )
        .unwrap();
        std::fs::write(memory_dir.join("topics").join("routing.md"), "topic secret").unwrap();

        let memory_store =
            MemoryStore::with_project_root(&root, root.join("messages"), "test-session");
        memory_store
            .append_transcript_message(&Message {
                role: "assistant".to_string(),
                content: Some("transcript secret".to_string()),
                tool_calls: None,
                tool_call_id: None,
            })
            .unwrap();

        let context = inject_project_context("inspect routing", &ProjectDocs::new(), &memory_store);

        assert!(context.contains("MEMORY INDEX"));
        assert!(context.contains("topics/routing.md"));
        assert!(!context.contains("topic secret"));
        assert!(!context.contains("transcript secret"));

        let _ = std::fs::remove_dir_all(root);
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
    tool_runtime: &ToolRuntime,
    depth: u32,
    spinner_label: &'static str,
    route_state: &mut RouteState,
    turn: &mut TurnState,
) -> Result<()> {
    /// Extra rounds granted each time the user chooses to keep going.
    const ROUND_EXTENSION: u32 = 24;

    // Reaching the round cap is no longer a hard error that throws away the whole
    // turn. The user can extend the budget; otherwise the model is asked for one
    // final tool-free wrap-up so progress so far is summarized instead of lost.
    let mut final_answer_only = false;
    if depth >= turn.max_rounds {
        println!(
            "{}",
            style(format!(
                "Tool loop reached {} rounds without finishing.",
                turn.max_rounds
            ))
            .yellow()
        );
        let extend = Confirm::new()
            .with_prompt(format!(
                "Let the agent continue for another {ROUND_EXTENSION} tool rounds?"
            ))
            .default(true)
            .interact()
            .unwrap_or(false);
        if extend {
            turn.max_rounds += ROUND_EXTENSION;
        } else {
            final_answer_only = true;
            conversation.add_user_message(
                "[Vybrid] The tool-round limit for this turn was reached. Do not call any more tools. Using the results gathered so far, summarize what was accomplished, what remains unfinished, and the exact next steps to continue.",
            );
            println!(
                "{}",
                style("Asking the model to wrap up without tools...").dim()
            );
        }
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
    // Only shrink the request after a genuine context-length rejection. Shrinking on
    // every retry used to rewrite the message prefix, invalidating Groq's prompt
    // cache exactly when rate limits made cached (free) tokens most valuable.
    let mut shrink_context = false;
    let mut last_usage: Option<Usage>;

    let mut attempt: u32 = 0;
    'stream: loop {
        attempt += 1;

        reasoning_started = false;
        content_started = false;
        final_content.clear();
        tool_calls.clear();
        first_chunk = true;
        last_usage = None;

        let request_budget = if route_state.mode == RouteMode::Primary && !shrink_context {
            turn.context_token_budget
        } else {
            turn.retry_context_token_budget
        };

        let mut spinner = ui::SpinnerGuard::new(spinner_label);
        let request_client = route_state.client_for_request(client);
        let request_tools = if final_answer_only {
            None
        } else {
            route_state.tools_for_request()
        };
        let stream = {
            let request_messages =
                request_messages_for_route(conversation, request_budget, route_state);
            request_client
                .chat_stream(&request_messages, request_tools)
                .await
        };
        let stream = match stream {
            Ok(s) => s,
            Err(e) => {
                spinner.finish().await;
                if is_preflight_route_error(&e) {
                    if route_state.route_preflight_wait() {
                        println!(
                            "{}",
                            style(format!(
                                "Groq preflight would wait; routing next request to {} with compacted context...",
                                route_state.route_label(client)
                            ))
                            .yellow()
                        );
                        continue 'stream;
                    }
                    return Err(e);
                }
                if attempt < MAX_STREAM_ATTEMPTS && is_rate_limit_error(&e) {
                    let delay = rate_limit_retry_delay(&e);
                    let switched_route = route_state.record_rate_limit_wait();
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
                            "Rate limit reached — waiting {}s, then retrying...",
                            delay.as_secs()
                        ))
                        .yellow()
                    );
                    if switched_route {
                        println!(
                            "{}",
                            style(format!(
                                "Primary Groq route is rate limited; using {} for this response.",
                                route_state.route_label(client)
                            ))
                            .yellow()
                        );
                        continue 'stream;
                    }
                    let mut wait_spinner = ui::SpinnerGuard::new("rate limit");
                    tokio::time::sleep(delay).await;
                    wait_spinner.finish().await;
                    continue 'stream;
                }
                if attempt < MAX_STREAM_ATTEMPTS && is_context_length_error(&e) {
                    shrink_context = true;
                    println!(
                        "{}",
                        style(format!(
                            "Context was too large for the provider — retrying with a compacted {}k-token request...",
                            turn.retry_context_token_budget / 1_000
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
                    if let Some(usage) = chunk.effective_usage() {
                        last_usage = Some(*usage);
                    }
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
                        let switched_route = route_state.record_rate_limit_wait();
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
                                "Rate limit reached mid-stream — waiting {}s, then retrying...",
                                delay.as_secs()
                            ))
                            .yellow()
                        );
                        if switched_route {
                            println!(
                                "{}",
                                style(format!(
                                    "Primary Groq route is still rate limited; using {} for this response.",
                                    route_state.route_label(client)
                                ))
                                .yellow()
                            );
                            continue 'stream;
                        }
                        let mut wait_spinner = ui::SpinnerGuard::new("rate limit");
                        tokio::time::sleep(delay).await;
                        wait_spinner.finish().await;
                        continue 'stream;
                    }
                    if attempt < MAX_STREAM_ATTEMPTS
                        && should_retry_tool_generation_error(&e, content_started)
                    {
                        conversation.add_user_message(&corrective_tool_prompt(
                            &e,
                            attempt,
                            MAX_STREAM_ATTEMPTS,
                        ));
                        let retry_message = if content_started && is_failed_generation_error(&e) {
                            "Tool generation failed after partial output — retrying with smaller tool calls..."
                        } else {
                            "Tool call validation failed — retrying with a corrective prompt..."
                        };
                        println!("{}", style(retry_message).yellow());
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

    if let Some(usage) = &last_usage {
        ui::print_usage_line(usage, &route_state.route_label(client));
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
    if let Some(memory) = tool_runtime.memory.as_ref() {
        record_latest_memory_message(memory, conversation);
    }

    // Execute tool calls if any. After a final-answer-only round no tools were
    // offered, so any stray tool_calls are ignored rather than recursed on.
    if !tool_calls.is_empty() && !final_answer_only {
        if route_state.is_compound() {
            conversation.add_user_message(
                "[Vybrid] Compound returned tool calls, but local tool execution is disabled for Compound routes. Continue by summarizing the intended next local action for OSS/Qwen.",
            );
            route_state.mode = RouteMode::Fallback;
            Box::pin(process_ai_response(
                client,
                conversation,
                tool_runtime,
                depth + 1,
                spinner_label,
                route_state,
                turn,
            ))
            .await?;
            return Ok(());
        }
        ui::print_tool_execution(tool_calls.len());

        let executable: Vec<&ToolCall> = tool_calls
            .iter()
            .filter(|tc| !tc.function.name.is_empty())
            .collect();

        // Rounds made of purely read-only tools (multi-file reads, greps, metadata
        // lookups) run concurrently; anything that mutates state stays sequential.
        let run_concurrently = executable.len() > 1
            && executable
                .iter()
                .all(|tc| is_read_only_tool(&tc.function.name));

        let results: Vec<Result<String>> = if run_concurrently {
            for tool_call in &executable {
                ui::print_tool_call(&tool_call.function.name);
            }
            futures::future::join_all(executable.iter().map(|tc| {
                execute_tool_with_context(&tc.function.name, &tc.function.arguments, tool_runtime)
            }))
            .await
        } else {
            let mut results = Vec::with_capacity(executable.len());
            for tool_call in &executable {
                ui::print_tool_call(&tool_call.function.name);
                results.push(
                    execute_tool_with_context(
                        &tool_call.function.name,
                        &tool_call.function.arguments,
                        tool_runtime,
                    )
                    .await,
                );
            }
            results
        };

        for (tool_call, result) in executable.iter().zip(results) {
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

            // Add compacted tool result to conversation. The terminal still shows previews, but
            // the model should not carry large repeated tool payloads across every follow-up turn.
            let result_str = result.unwrap_or_else(|e| format!("Error: {}", e));
            let mut history_result =
                compact_tool_result_for_history(&tool_call.function.name, &result_str, depth);
            if let Some(nudge) = record_tool_call_repetition(
                &mut turn.repeated_tool_calls,
                &tool_call.function.name,
                &tool_call.function.arguments,
            ) {
                history_result.push_str(&nudge);
            }
            conversation.add_tool_result(&tool_call.id, &history_result);
            if let Some(memory) = tool_runtime.memory.as_ref() {
                record_latest_memory_message(memory, conversation);
            }
        }

        // Get follow-up response (next round shows its own spinner)
        println!();
        Box::pin(process_ai_response(
            client,
            conversation,
            tool_runtime,
            depth + 1,
            spinner_label,
            route_state,
            turn,
        ))
        .await?;
    }

    Ok(())
}

fn compact_tool_result_for_history(tool_name: &str, result: &str, depth: u32) -> String {
    let max_chars = match tool_name {
        "read_file" => 12_000,
        "read_multiple_files" => 10_000,
        "enhanced_grep" => 8_000,
        "run_cargo" => 16_000,
        "execute_bash_command" => 10_000,
        _ => 8_000,
    };
    let max_chars = if depth >= 2 {
        (max_chars / 2).max(4_000)
    } else {
        max_chars
    };

    let total_chars = result.chars().count();
    if total_chars <= max_chars {
        return result.to_string();
    }

    let truncated = conversation::truncate_middle(result, max_chars, "omitted middle of");
    format!(
        "[Vybrid compacted `{tool_name}` result for history: original {total_chars} chars, kept ~{max_chars} chars. Re-read a narrower range if exact omitted content is needed.]\n\n{truncated}"
    )
}

fn request_messages_for_route<'a>(
    conversation: &'a mut Conversation,
    request_budget: u32,
    route_state: &RouteState,
) -> Cow<'a, [Message]> {
    if route_state.is_compound() {
        return Cow::Owned(compound_messages_for_request(conversation, request_budget));
    }
    conversation.messages_for_request_with_budget(request_budget)
}

fn compound_messages_for_request(
    conversation: &mut Conversation,
    request_budget: u32,
) -> Vec<Message> {
    let source = conversation.messages_for_request_with_budget(request_budget);
    let system = source
        .first()
        .filter(|m| m.role == "system")
        .cloned()
        .unwrap_or_else(|| Message {
            role: "system".to_string(),
            content: Some("You are a concise planning assistant.".to_string()),
            tool_calls: None,
            tool_call_id: None,
        });

    let mut transcript = String::new();
    for message in source
        .iter()
        .skip(1)
        .rev()
        .take(12)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
    {
        let role = message.role.as_str();
        let content = message.content.as_deref().unwrap_or("");
        if content.trim().is_empty() && message.tool_calls.is_none() {
            continue;
        }
        transcript.push_str(&format!("\n\n[{role}]\n"));
        if !content.trim().is_empty() {
            transcript.push_str(content);
        }
        if let Some(tool_calls) = &message.tool_calls {
            let names = tool_calls
                .iter()
                .map(|tc| tc.function.name.as_str())
                .filter(|name| !name.is_empty())
                .collect::<Vec<_>>();
            if !names.is_empty() {
                transcript.push_str(&format!("\nTool calls requested: {}", names.join(", ")));
            }
        }
    }

    let max_chars = (request_budget as usize).saturating_mul(4).min(24_000);
    if transcript.chars().count() > max_chars {
        transcript = transcript
            .chars()
            .rev()
            .take(max_chars)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        transcript.insert_str(0, "[Older transcript omitted for Compound route]\n");
    }

    vec![
        system,
        Message {
            role: "user".to_string(),
            content: Some(format!(
                "Vybrid routed this turn to Compound because local tool-calling models were near TPM limits. You do not have access to Vybrid's local filesystem tools in this route.\n\nUse the transcript below to provide a concise planning/summarization response or the next local-tool action Vybrid should take. Do not claim files were edited or commands were run.\n{transcript}"
            )),
            tool_calls: None,
            tool_call_id: None,
        },
    ]
}

fn record_latest_memory_message(memory_store: &MemoryStore, conversation: &Conversation) {
    if let Some(message) = conversation.messages.last() {
        if let Err(e) = memory_store.append_transcript_message(message) {
            eprintln!("{}", style(format!("Memory transcript warning: {e}")).dim());
        }
    }
}

/// Inject project documentation and the compact memory index into a user message.
fn inject_project_context(
    user_message: &str,
    project_docs: &ProjectDocs,
    memory_store: &MemoryStore,
) -> String {
    let with_docs = inject_project_docs(user_message, project_docs);
    match memory_store.context_block() {
        Ok(Some(memory)) => format!("{with_docs}\n\n---\n\n{memory}"),
        Ok(None) | Err(_) => with_docs,
    }
}

/// Inject project documentation context into user message
fn inject_project_docs(user_message: &str, project_docs: &ProjectDocs) -> String {
    const MAX_PROJECT_DOC_CHARS: usize = 8_000;
    let index_hint = if project_index::index_path().exists() {
        "\n\n---\n\nPROJECT INDEX: `index.md` exists at the project root. Read it with `read_project_index` when project navigation is needed."
    } else {
        ""
    };
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
            format!(
                "{}{index_hint}\n\n---\n\nPROJECT CONTEXT:\n{}",
                user_message, docs
            )
        }
        Ok(None) | Err(_) => format!("{user_message}{index_hint}"),
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

PROJECT NAVIGATION POLICY:
1. Treat paths as project-root-relative unless the user gives an absolute path. Never concatenate absolute and relative project paths.
2. Before broad exploration of an unfamiliar or large project, read `index.md` with `read_project_index`. If it is missing or stale, use `generate_project_index` instead of repeated broad `ls`, glob, or full-file reads.
3. Use paths exactly as listed in `index.md` or tool output. Prefer targeted `read_file` line ranges, `rust_project_snapshot`, and narrow `enhanced_grep` calls.

SKEPTICAL MEMORY POLICY:
1. Treat `MEMORY.md` as a compact index of pointers, not as authoritative truth.
2. Read memory topics only when the index suggests they are relevant to the current task.
3. Search raw transcripts only for specific identifiers, file paths, symbols, or error codes; never load or summarize transcripts wholesale.
4. Before acting on remembered paths, symbols, commands, dependencies, or behavior, verify them against live project tools such as `read_file`, `enhanced_grep`, `rust_project_snapshot`, `cargo_metadata`, or compiler output.
5. autoDream may consolidate completed-session transcripts into `topics/autodream.md` after idle gates pass; treat that topic as a compressed hint layer and verify it like any other memory.

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
- read_project_index, generate_project_index: Read or generate compact `index.md` navigation context before broad project discovery
- list_memory_topics, read_memory_topic, search_memory_transcripts: Use the three-layer memory system sparingly; memory results are hints and must be verified against live project state before acting
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
5. File reads are metadata-cached within a session. If a repeated read says `cache: hit`, treat it as the unchanged file content from the previous read.
6. Large tool results may be offloaded to disk with a preview and `Full result` path. Use `read_file` on that path only when exact omitted content is needed.

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
        "  {}     - Menu (Groq / OpenRouter / LM Studio / SerpAPI / Rust LSP)",
        style("/menu").yellow()
    );
    println!(
        "  {}      - Show or refresh compact project index",
        style("/index").yellow()
    );
    println!(
        "  {}  - Show or set thinking level: /thinking low|medium|high|default",
        style("/thinking").yellow()
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

fn handle_thinking_command(
    input: &str,
    config: &mut Config,
    client: &mut Option<GroqClient>,
) {
    let parts: Vec<&str> = input.split_whitespace().collect();
    let model = config.active_model_id();

    match parts.get(1).map(|s| s.to_ascii_lowercase()) {
        None => {
            let configured = config
                .reasoning_effort
                .as_deref()
                .unwrap_or("default (provider)");
            let active = crate::config::effective_reasoning_effort(
                &model,
                config.reasoning_effort.as_deref(),
            )
            .unwrap_or("not sent");
            let supported = if crate::config::model_supports_thinking_levels(&model) {
                "yes"
            } else {
                "no"
            };
            println!();
            println!("{}", style("Thinking level").cyan().bold());
            println!("{}", style("─".repeat(40)).dim());
            println!("  Model:      {}", model);
            println!("  Configured: {}", configured);
            println!("  Active:     {}", active);
            println!("  Supported:  {}", supported);
            println!(
                "  {}",
                style("Use /thinking low|medium|high or /thinking default").dim()
            );
            println!();
        }
        Some(level) => match level.as_str() {
            "low" | "medium" | "high" | "default" => {
                let effort = if level == "default" {
                    None
                } else {
                    Some(level.as_str())
                };
                match config.set_reasoning_effort(effort) {
                    Ok(()) => {
                        *client = config.build_chat_client();
                        let indicator =
                            crate::config::format_thinking_indicator(&model, config.reasoning_effort.as_deref());
                        println!(
                            "{}",
                            style(format!("Thinking level updated — {indicator} (model: {model})"))
                                .green()
                        );
                        if config.reasoning_effort.is_some()
                            && crate::config::effective_reasoning_effort(
                                &model,
                                config.reasoning_effort.as_deref(),
                            )
                            .is_none()
                        {
                            ui::print_error(
                                "This model may not support low/medium/high thinking; requests will use the provider default.",
                            );
                        }
                    }
                    Err(e) => ui::print_error(&format!("{}", e)),
                }
            }
            _ => ui::print_error(
                "Usage: /thinking [low|medium|high|default] — run /thinking alone to show status",
            ),
        },
    }
}

fn handle_index_command(input: &str) {
    let parts: Vec<&str> = input.split_whitespace().collect();
    match parts.get(1).copied() {
        None | Some("show") => match project_index::read_project_index() {
            Ok(content) => {
                println!();
                println!("{}", style("Project Index").cyan().bold());
                println!("{}", style("─".repeat(40)).dim());
                println!("{}", content);
                println!();
            }
            Err(e) => ui::print_error(&format!("Failed to read project index: {e}")),
        },
        Some("refresh") | Some("generate") => match project_index::generate_project_index(true) {
            Ok(msg) => println!("{}", style(msg).green()),
            Err(e) => ui::print_error(&format!("Failed to generate project index: {e}")),
        },
        Some("path") => println!("{}", project_index::index_path().display()),
        Some(other) => {
            ui::print_error(&format!("Unknown /index subcommand: '{other}'"));
            println!("Available subcommands: show, refresh, path");
        }
    }
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

