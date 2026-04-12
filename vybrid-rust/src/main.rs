mod client;
mod config;
mod conversation;
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
use crate::config::Config;
use crate::conversation::Conversation;
use crate::project_docs::ProjectDocs;
use crate::tools::definitions::get_all_tools;
use crate::tools::executor::execute_tool;

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

    let mut client = config.api_key.as_ref().map(|key| {
        GroqClient::new(
            key.clone(),
            config.api_base_url.clone(),
            config.model.clone(),
        )
    });

    if client.is_none() {
        println!(
            "{}",
            style(
                "No Groq API key found. After you add keys once, they are saved to ~/.vybrid/.env and vybrid-rust/.env (kept in sync) so Vybrid works from any directory."
            )
            .dim()
        );
        println!();
        match Confirm::new()
            .with_prompt("Open setup menu to add API keys now?")
            .default(true)
            .interact()
        {
            Ok(true) => {
                if let Err(e) = handle_menu(&mut config, &mut client) {
                    ui::print_error(&format!("{}", e));
                }
            }
            Ok(false) | Err(_) => {}
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

    loop {
        ui::print_context_status_line(conversation.estimate_context_tokens());
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
                if let Err(e) = handle_menu(&mut config, &mut client) {
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
            handle_docs_command(&input, &project_docs);
            continue;
        }

        // Handle single shell commands (!command)
        if input.starts_with('!') {
            let cmd = &input[1..];
            match tools::shell::execute_bash(cmd, None, None).await {
                Ok(output) => println!("{}", output),
                Err(e) => ui::print_error(&format!("Command failed: {}", e)),
            }
            continue;
        }

        // Handle /add command
        if input.starts_with("/add ") {
            let path = &input[5..];
            match tools::file_ops::read_file(path) {
                Ok(content) => {
                    conversation.add_user_message(&format!("I'm adding this file to our conversation:\n\n{}", content));
                    println!("{}", style(format!("Added '{}' to conversation", path)).green());
                }
                Err(e) => ui::print_error(&format!("Failed to read '{}': {}", path, e)),
            }
            continue;
        }

        let Some(ref c) = client else {
            ui::print_error(&format!(
                "No Groq API key. Use /menu — keys are saved to {} and {}.",
                config.global_env_file_path.display(),
                config.env_file_path.display()
            ));
            continue;
        };

        // Add user message to conversation with project docs context
        let user_message_with_context = inject_project_docs(&input, &project_docs);
        conversation.add_user_message(&user_message_with_context);

        let tools = get_all_tools();

        // Process with AI
        if let Err(e) = process_ai_response(c, &mut conversation, &tools, 0).await {
            ui::print_error(&format!("AI error: {}", e));
        }

        println!();
    }

    Ok(())
}

/// Groq may reject a tool call before streaming any assistant text; these errors are worth one automatic retry.
fn is_retryable_groq_stream_error(e: &anyhow::Error) -> bool {
    let s = e.to_string();
    s.contains("tool_use_failed")
        || s.contains("did not match schema")
        || s.contains("Tool call validation")
}

/// Process AI response with streaming and tool calls
async fn process_ai_response(
    client: &GroqClient,
    conversation: &mut Conversation,
    tools: &[crate::client::groq::Tool],
    depth: u32,
) -> Result<()> {
    /// Prevents runaway tool loops when the model chains many rounds.
    const MAX_TOOL_ROUNDS: u32 = 48;
    if depth >= MAX_TOOL_ROUNDS {
        anyhow::bail!(
            "Stopped: tool loop exceeded {} rounds. Try /new or a smaller task.",
            MAX_TOOL_ROUNDS
        );
    }

    /// If the API rejects a tool call before any tokens arrive, retry once with a corrective user note.
    const MAX_STREAM_ATTEMPTS: u32 = 2;

    let mut reasoning_started: bool;
    let mut content_started: bool;
    let mut final_content = String::new();
    let mut tool_calls: Vec<ToolCall> = Vec::new();
    let mut first_chunk: bool;

    let mut attempt: u32 = 0;
    'stream: loop {
        attempt += 1;

        reasoning_started = false;
        content_started = false;
        final_content.clear();
        tool_calls.clear();
        first_chunk = true;

        let mut spinner = ui::SpinnerGuard::new("groq");
        let stream = client
            .chat_stream(conversation.get_messages(), Some(tools.to_vec()))
            .await;
        let stream = match stream {
            Ok(s) => s,
            Err(e) => {
                spinner.finish().await;
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
                                        tool_calls[tc_delta.index].function.arguments.push_str(args);
                                    }
                                }
                            }
                        }
                    }
                }
                Err(e) => {
                    println!();
                    if attempt < MAX_STREAM_ATTEMPTS
                        && is_retryable_groq_stream_error(&e)
                        && !content_started
                        && !reasoning_started
                    {
                        conversation.add_user_message(&format!(
                            "[Vybrid] The API rejected a tool call before the reply could start: {e}. This was attempt {attempt} of {MAX_STREAM_ATTEMPTS}. Please respond again; use tools only with arguments that match each tool's schema (see descriptions). For enhanced_grep, include `pattern` plus `path`, `file_path`, or `file_paths`."
                        ));
                        println!(
                            "{}",
                            style(
                                "Groq tool validation failed — retrying once with a corrective prompt…"
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

            let result = execute_tool(&tool_call.function.name, &tool_call.function.arguments).await;

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
            depth + 1,
        ))
        .await?;
    }

    Ok(())
}

/// Inject project documentation context into user message
fn inject_project_docs(user_message: &str, project_docs: &ProjectDocs) -> String {
    match project_docs.read() {
        Ok(Some(docs)) => {
            format!(
                "{}\n\n---\n\nPROJECT CONTEXT:\n{}",
                user_message, docs
            )
        }
        Ok(None) | Err(_) => user_message.to_string(),
    }
}

/// Get the system prompt for agent mode
fn get_system_prompt() -> String {
    format!(
        r#"You are Vybrid, an elite software engineer with decades of experience across all programming domains and EXPERT-LEVEL MASTERY OF RUST PROGRAMMING.
Your Rust expertise includes ownership/borrowing, lifetimes, async/await patterns, trait systems, error handling with anyhow/thiserror, and idiomatic Rust code design.
You provide thoughtful, well-structured solutions while explaining your reasoning, with particular strength in Rust-specific best practices.

RUST EXPERTISE HIGHLIGHT:
- Deep knowledge of Rust ownership model, borrowing rules, and lifetime annotations
- Proficiency with async Rust using tokio, futures, and async-stream
- Mastery of Rust trait system, generics, and type-level programming
- Expert in error handling: Result<T>, anyhow::Context, thiserror for custom errors
- Familiarity with Rust ecosystem: serde, reqwest, tokio, clap, and common crates
- Experience with performance optimization, zero-cost abstractions, and unsafe code when needed
- Understanding of Rust project structure, Cargo.toml configuration, and workspace patterns
- Knowledge of Rust idioms: iterators, Option/Result combinators, pattern matching

CRITICAL WORKFLOW REQUIREMENTS:
You MUST follow these development rules for every project:

1. PROJECT STRUCTURE SETUP (Always create these files first):
   - `tasks/todo.md` - Structured task tracking with checkboxes
   - `docs/activity.md` - Activity logging with timestamps
   - `docs/PROJECT_README.md` - Project context for AI agents (MANDATORY)
   - Create these files IMMEDIATELY when starting any project work

2. CONTINUOUS DEVELOPMENT WORKFLOW:
   - Step 1: Read and understand the problem/codebase
   - Step 2: Create all three mandatory files (todo.md, activity.md, PROJECT_README.md)
   - Step 3: Create structured plan in `tasks/todo.md` using checkboxes
   - Step 4: IMMEDIATELY begin executing tasks - DO NOT wait for approval
   - Step 5: Execute tasks sequentially, marking complete with [x]
   - Step 6: Log all actions in `docs/activity.md` with timestamps
   - Step 7: Continue until all tasks are complete or user stops you

3. MANDATORY FILE FORMATS:
   - Todo items must use checkboxes: `- [ ] Task description`
   - Activity log must include timestamps: `## YYYY-MM-DD HH:MM - Action taken`
   - Keep todo items granular and testable
   - Log every significant action

RUST CARGO QUICK REFERENCE:
{}

COMPILE / FIX LOOP:
{}

COMMON RUST DIAGNOSTICS:
{}

Available tools:
- read_file, read_multiple_files: Read file contents
- create_file, create_multiple_files: Create or overwrite files
- edit_file: Make precise edits using snippet replacement
- run_cargo: Run Cargo (check, build, test, clippy, fmt, doc, …) with structured argv — **preferred for Rust projects** over raw shell when invoking cargo
- execute_bash_command: Run shell commands (rustup, system packages, non-cargo scripts)
- enhanced_grep: Search files with regex patterns
- google_search: Search for information online
- create_project_structure: Initialize project files
- get_current_todo_items: List incomplete tasks
- mark_todo_complete: Mark a task as done

Guidelines:
1. ALWAYS start any project work by creating project structure files
2. Read files before editing to understand context
3. Use precise snippet matching for edits
4. On Rust projects, use run_cargo for compile/test/lint iteration; read full compiler output before fixing
5. Explain what you're doing and why
6. Be thorough in analysis and recommendations

IMPORTANT: Execute tasks immediately - don't wait for approval. Be efficient and thorough."#,
        crate::rust_agent_reference::RUST_CARGO_QUICKREF,
        crate::rust_agent_reference::RUST_COMPILE_FIX_LOOP,
        crate::rust_agent_reference::RUST_DIAGNOSTICS_HINTS,
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
        let desc: String = tool.function.description.lines().next().unwrap_or("").to_string();
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
    println!("  {}       - Enter persistent shell mode", style("!").yellow());
    println!("  {}    - Execute single shell command", style("!<cmd>").yellow());
    println!("  {} - Add file to conversation", style("/add <path>").yellow());
    println!("  {}       - Show current directory", style("/pwd").yellow());
    println!("  {}     - List available AI tools", style("/tools").yellow());
    println!("  {}       - Start new conversation", style("/new").yellow());
    println!("  {}     - Clear screen", style("clear").yellow());
    println!("  {}      - Show this help", style("/help").yellow());
    println!(
        "  {}     - Menu (Groq & SerpAPI → ~/.vybrid/.env + vybrid-rust/.env)",
        style("/menu").yellow()
    );
    println!();
    println!("{}", style("Project Docs Commands:").cyan().bold());
    println!("{}", style("─".repeat(40)).dim());
    println!("  {}           - Show current project docs", style("/docs").yellow());
    println!("  {}  - Add docs from a file", style("/docs add <file>").yellow());
    println!("  {}          - Read docs interactively", style("/docs read").yellow());
    println!("  {}         - Clear project docs", style("/docs clear").yellow());
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
                    println!(
                        "Create one with: {}",
                        style("/docs read").yellow()
                    );
                    println!(
                        "Or add from file: {}",
                        style("/docs add <file>").yellow()
                    );
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
                    Ok(content) => {
                        match project_docs.add(&content) {
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
                        }
                    }
                    Err(e) => {
                        ui::print_error(&format!("Failed to read '{}': {}", path, e));
                    }
                }
            } else {
                println!(
                    "{}",
                    style("Usage: /docs add <file>").dim()
                );
            }
        }
        Some("read") => {
            // Read docs interactively
            println!();
            println!("{}", style("Enter project documentation (empty line to finish):").cyan().bold());
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
                    println!(
                        "{}",
                        style("Project documentation saved.").green()
                    );
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
            println!("{}", style(format!("Unknown /docs subcommand: '{}'", unknown)).red());
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

/// Interactive menu — keys written to `~/.vybrid/.env` and `vybrid-rust/.env`
fn handle_menu(config: &mut Config, client: &mut Option<GroqClient>) -> Result<()> {
    let items = vec![
        "Add both Groq + SerpAPI keys (SerpAPI optional — then save & use chat)",
        "Add or update Groq API key only",
        "Add or update SerpAPI key only (Google search)",
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
            let api_key = config
                .api_key
                .clone()
                .context("API key missing after save")?;
            *client = Some(GroqClient::new(
                api_key,
                config.api_base_url.clone(),
                config.model.clone(),
            ));

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
            let api_key = config
                .api_key
                .clone()
                .context("API key missing after save")?;
            *client = Some(GroqClient::new(
                api_key,
                config.api_base_url.clone(),
                config.model.clone(),
            ));
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
        _ => {}
    }
    Ok(())
}
