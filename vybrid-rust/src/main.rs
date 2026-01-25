mod client;
mod config;
mod conversation;
mod daemon;
mod shell;
mod tools;
mod ui;

use anyhow::Result;
use console::style;
use dialoguer::Select;
use futures::StreamExt;
use std::io::{self, Write};

use crate::client::glm::{GlmClient, Message, ToolCall};
use crate::config::Config;
use crate::conversation::Conversation;
use crate::tools::definitions::get_all_tools;
use crate::tools::executor::execute_tool;

/// Application mode
#[derive(Debug, Clone, Copy, PartialEq)]
enum Mode {
    Agent,
    Daemon,
}

#[tokio::main]
async fn main() -> Result<()> {
    // Load configuration
    let config = match Config::load() {
        Ok(c) => c,
        Err(e) => {
            ui::print_error(&format!("Configuration error: {}", e));
            eprintln!("\nPlease create ~/.vybrid/.env with:");
            eprintln!("  ZAI_API_KEY=your_api_key_here");
            eprintln!("\nOr create a local .env file in your project directory.");
            std::process::exit(1);
        }
    };

    // Display banner
    ui::display_banner();

    // Mode selection
    let mode = select_mode()?;

    match mode {
        Mode::Agent => run_agent_mode(config).await,
        Mode::Daemon => run_daemon_mode(config).await,
    }
}

/// Interactive mode selection
fn select_mode() -> Result<Mode> {
    println!();
    let options = &[
        "[A] Agent Mode - Full AI Engineer with tools",
        "[D] Daemon Mode - Background service for processing requests",
    ];

    let selection = Select::new()
        .with_prompt("Select mode")
        .items(options)
        .default(0)
        .interact()?;

    Ok(match selection {
        0 => Mode::Agent,
        1 => Mode::Daemon,
        _ => Mode::Agent,
    })
}

/// Run agent mode - interactive AI assistant
async fn run_agent_mode(config: Config) -> Result<()> {
    ui::display_mode_header("agent");
    ui::display_cwd();

    let client = GlmClient::new(
        config.api_key.clone(),
        config.api_base_url.clone(),
        config.model.clone(),
    );

    let tools = get_all_tools();
    let mut conversation = Conversation::new(&get_system_prompt());

    loop {
        // Prompt for input
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
                ui::display_mode_header("agent");
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

        // Add user message to conversation
        conversation.add_user_message(input);

        // Process with AI
        if let Err(e) = process_ai_response(&client, &mut conversation, &tools).await {
            ui::print_error(&format!("AI error: {}", e));
        }

        println!();
    }

    Ok(())
}

/// Process AI response with streaming and tool calls
async fn process_ai_response(
    client: &GlmClient,
    conversation: &mut Conversation,
    tools: &[crate::client::glm::Tool],
) -> Result<()> {
    let stream = client
        .chat_stream(conversation.get_messages(), Some(tools.to_vec()))
        .await?;

    futures::pin_mut!(stream);

    let mut reasoning_started = false;
    let mut content_started = false;
    let mut final_content = String::new();
    let mut tool_calls: Vec<ToolCall> = Vec::new();

    while let Some(chunk_result) = stream.next().await {
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
                                    function: crate::client::glm::FunctionCall {
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
                return Err(e);
            }
        }
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
                    if tool_call.function.name == "execute_bash_command" {
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

        // Get follow-up response
        println!();
        ui::print_info("Processing results...");

        // Recursive call for follow-up (with depth limit built into the loop)
        Box::pin(process_ai_response(client, conversation, tools)).await?;
    }

    Ok(())
}

/// Run daemon mode - background service
async fn run_daemon_mode(config: Config) -> Result<()> {
    ui::display_mode_header("daemon");
    daemon::start_daemon_pool(config).await
}

/// Get the system prompt for agent mode
fn get_system_prompt() -> String {
    r#"You are Vybrid, an elite software engineer with decades of experience across all programming domains.
Your expertise spans system design, algorithms, testing, and best practices.
You provide thoughtful, well-structured solutions while explaining your reasoning.

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

Available tools:
- read_file, read_multiple_files: Read file contents
- create_file, create_multiple_files: Create or overwrite files
- edit_file: Make precise edits using snippet replacement
- execute_bash_command: Run shell commands
- enhanced_grep: Search files with regex patterns
- google_search: Search for information online
- create_project_structure: Initialize project files
- get_current_todo_items: List incomplete tasks
- mark_todo_complete: Mark a task as done

Guidelines:
1. ALWAYS start any project work by creating project structure files
2. Read files before editing to understand context
3. Use precise snippet matching for edits
4. Explain what you're doing and why
5. Be thorough in analysis and recommendations

IMPORTANT: Execute tasks immediately - don't wait for approval. Be efficient and thorough."#.to_string()
}

/// Show available tools
fn show_available_tools() {
    println!();
    println!("{}", style("Available Tools:").cyan().bold());
    println!("{}", style("─".repeat(40)).dim());

    let tools = get_all_tools();
    for tool in tools {
        println!(
            "  {} - {}",
            style(&tool.function.name).yellow(),
            style(&tool.function.description).dim()
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
    println!();
}
