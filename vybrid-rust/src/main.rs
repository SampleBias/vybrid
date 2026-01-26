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
use std::sync::Arc;

use crate::client::glm::{GlmClient, Message, ToolCall};
use crate::config::Config;
use crate::conversation::Conversation;
use crate::daemon::queue::{ExecutionRequest, MessageQueue};
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

    // Initialize message queue for daemon communication
    let message_queue = Arc::new(MessageQueue::new(
        config.messages_dir.clone(),
        config.progress_dir.clone(),
    ));

    // Check initial daemon availability and show status
    let daemon_available = config.is_daemon_available();
    if daemon_available {
        println!("{}", style("Daemon pool detected - delegation tools enabled").green().dim());
    } else {
        println!("{}", style("Daemon pool not running - delegation tools disabled").yellow().dim());
    }
    println!();

    let mut conversation = Conversation::new(&get_system_prompt(daemon_available));

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
                show_available_tools(config.is_daemon_available());
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

        // Handle /delegate command - send task to daemon
        if input.starts_with("/delegate ") {
            let task = &input[10..];
            if task.trim().is_empty() {
                ui::print_error("Usage: /delegate <task description>");
                continue;
            }
            
            match delegate_to_daemon(&message_queue, task).await {
                Ok(result) => {
                    println!("\n{}", style("Daemon Response:").cyan().bold());
                    println!("{}", style("─".repeat(50)).dim());
                    println!("{}", result);
                    println!("{}", style("─".repeat(50)).dim());
                    
                    // Optionally add result to conversation context
                    conversation.add_user_message(&format!(
                        "I delegated this task to the daemon: \"{}\"\n\nResult:\n{}",
                        task, result
                    ));
                }
                Err(e) => ui::print_error(&format!("Delegation failed: {}", e)),
            }
            continue;
        }

        // Handle /daemon-status command
        if input == "/daemon" || input == "/daemon-status" {
            check_daemon_status(&config);
            continue;
        }

        // Add user message to conversation
        conversation.add_user_message(input);

        // Check daemon availability before each AI request (dynamic tool availability)
        let daemon_available = config.is_daemon_available();
        let tools = get_all_tools(daemon_available);

        // Process with AI
        if let Err(e) = process_ai_response(&client, &mut conversation, &tools, &config).await {
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
    config: &Config,
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

            let result = execute_tool(&tool_call.function.name, &tool_call.function.arguments, Some(config)).await;

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
        Box::pin(process_ai_response(client, conversation, tools, config)).await?;
    }

    Ok(())
}

/// Run daemon mode - background service
async fn run_daemon_mode(config: Config) -> Result<()> {
    ui::display_mode_header("daemon");
    daemon::start_daemon_pool(config).await
}

/// Delegate a task to the daemon pool
async fn delegate_to_daemon(queue: &Arc<MessageQueue>, task: &str) -> Result<String> {
    // Check if daemon is running by looking for the lock file
    let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("Could not find home directory"))?;
    let lock_file = home.join(".vybrid").join("daemon_pool").join("pool.lock");
    
    if !lock_file.exists() {
        return Err(anyhow::anyhow!(
            "Daemon is not running. Start it with: vybrid → Daemon Mode"
        ));
    }

    // Get current working directory
    let cwd = std::env::current_dir()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| ".".to_string());

    // Create execution request
    let request = ExecutionRequest::new(task.to_string(), cwd);
    let request_id = request.id.clone();

    println!(
        "\n{} Delegating to daemon...",
        style("→").cyan()
    );
    println!("  Request ID: {}", style(&request_id[..8]).yellow());
    println!("  Task: {}", style(task).dim());

    // Send request to queue
    queue.send_request(&request)?;
    println!("  {} Request sent", style("✓").green());

    // Poll for response with progress updates
    println!("  {} Waiting for daemon response...", style("⏳").yellow());

    let timeout_secs = 300; // 5 minute timeout
    let start = std::time::Instant::now();
    let poll_interval = std::time::Duration::from_millis(500);
    let mut last_stage = String::new();

    loop {
        // Check timeout
        if start.elapsed().as_secs() >= timeout_secs {
            return Err(anyhow::anyhow!("Daemon response timeout after {} seconds", timeout_secs));
        }

        // Check for response file
        let response_path = home
            .join(".vybrid")
            .join("messages")
            .join(format!("response_{}.json", request_id));

        if response_path.exists() {
            // Read and parse response
            let content = std::fs::read_to_string(&response_path)?;
            let response: crate::daemon::queue::ExecutionResponse = serde_json::from_str(&content)?;

            // Clean up request file
            let request_path = home
                .join(".vybrid")
                .join("messages")
                .join(format!("request_{}.json", request_id));
            let _ = std::fs::remove_file(&request_path);
            let _ = std::fs::remove_file(&response_path);

            // Clean up progress file
            let progress_path = home
                .join(".vybrid")
                .join("progress")
                .join(format!("progress_{}.json", request_id));
            let _ = std::fs::remove_file(&progress_path);

            match response.status.as_str() {
                "success" => {
                    println!(
                        "  {} Completed in {:.2}s",
                        style("✓").green(),
                        response.processing_time.unwrap_or(0.0)
                    );
                    return Ok(response.result);
                }
                "error" => {
                    return Err(anyhow::anyhow!("Daemon error: {}", response.result));
                }
                "cancelled" => {
                    return Err(anyhow::anyhow!("Request was cancelled"));
                }
                _ => {
                    return Err(anyhow::anyhow!("Unknown response status: {}", response.status));
                }
            }
        }

        // Check for progress updates
        let progress_path = home
            .join(".vybrid")
            .join("progress")
            .join(format!("progress_{}.json", request_id));

        if progress_path.exists() {
            if let Ok(content) = std::fs::read_to_string(&progress_path) {
                if let Ok(progress) = serde_json::from_str::<crate::daemon::queue::ProgressUpdate>(&content) {
                    if progress.stage != last_stage {
                        last_stage = progress.stage.clone();
                        if let Some(msg) = &progress.message {
                            println!("  {} {} - {}", style("↻").blue(), progress.stage, style(msg).dim());
                        } else {
                            println!("  {} {}", style("↻").blue(), progress.stage);
                        }
                    }
                }
            }
        }

        tokio::time::sleep(poll_interval).await;
    }
}

/// Check if daemon is running
fn check_daemon_status(config: &Config) {
    let lock_file = config.daemon_lock_file();
    
    println!();
    println!("{}", style("Daemon Status:").cyan().bold());
    println!("{}", style("─".repeat(40)).dim());

    if !lock_file.exists() {
        println!("  Status: {} Not running", style("●").red());
        println!("  Start with: {} → select Daemon Mode", style("vybrid").yellow());
    } else {
        match std::fs::read_to_string(&lock_file) {
            Ok(content) => {
                if let Ok(lock) = serde_json::from_str::<serde_json::Value>(&content) {
                    println!("  Status: {} Running", style("●").green());
                    if let Some(pid) = lock.get("pid") {
                        println!("  PID: {}", style(pid).yellow());
                    }
                    if let Some(workers) = lock.get("workers") {
                        println!("  Workers: {}", style(workers).yellow());
                    }
                    if let Some(session) = lock.get("session_id").and_then(|s| s.as_str()) {
                        println!("  Session: {}", style(&session[..8]).yellow());
                    }
                    if let Some(timestamp) = lock.get("timestamp").and_then(|t| t.as_str()) {
                        println!("  Started: {}", style(timestamp).dim());
                    }
                }
            }
            Err(_) => {
                println!("  Status: {} Unknown (lock file unreadable)", style("●").yellow());
            }
        }
    }

    println!();
    println!("{}", style("Usage:").cyan());
    println!("  {} - Send task to daemon", style("/delegate <task>").yellow());
    println!();
}

/// Get the system prompt for agent mode
fn get_system_prompt(daemon_available: bool) -> String {
    let base_prompt = r#"You are Vybrid, an elite software engineer with decades of experience across all programming domains.
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
- mark_todo_complete: Mark a task as done"#;

    let daemon_section = if daemon_available {
        r#"

DAEMON DELEGATION (AVAILABLE):
The daemon pool is running with background workers. You have access to delegation tools:
- delegate_to_daemon: Send tasks to background workers for parallel/async execution
- check_daemon_status: Check daemon pool status and worker availability

DELEGATION GUIDELINES:
Use delegate_to_daemon for:
• Long-running operations (builds, tests, large file processing)
• Multiple independent tasks that can run in parallel
• Background work while continuing to interact with the user
• Tasks that don't need immediate results

Execute directly (without delegation) for:
• Quick file reads/writes
• Simple grep searches  
• Single commands with immediate results
• Tasks where you need the result before proceeding

DELEGATION BEST PRACTICES:
1. For parallel work: Delegate multiple tasks with wait_for_result=false, then check status
2. For sequential work: Use wait_for_result=true (default)
3. Set appropriate priority (1=urgent, 5=background)
4. Provide clear, detailed task descriptions - daemon workers have full context"#
    } else {
        r#"

DAEMON DELEGATION (NOT AVAILABLE):
The daemon pool is not currently running. Delegation tools are disabled.
All tasks will be executed directly in this session.

To enable delegation:
1. Start a new terminal
2. Run: vybrid → select Daemon Mode
3. Return to this session - delegation tools will become available"#
    };

    let guidelines = r#"

Guidelines:
1. ALWAYS start any project work by creating project structure files
2. Read files before editing to understand context
3. Use precise snippet matching for edits
4. Explain what you're doing and why
5. Be thorough in analysis and recommendations

IMPORTANT: Execute tasks immediately - don't wait for approval. Be efficient and thorough."#;

    format!("{}{}{}", base_prompt, daemon_section, guidelines)
}

/// Show available tools
fn show_available_tools(daemon_available: bool) {
    println!();
    println!("{}", style("Available Tools:").cyan().bold());
    println!("{}", style("─".repeat(40)).dim());

    let tools = get_all_tools(daemon_available);
    for tool in tools {
        // Truncate description for display
        let desc: String = tool.function.description.lines().next().unwrap_or("").to_string();
        println!(
            "  {} - {}",
            style(&tool.function.name).yellow(),
            style(&desc).dim()
        );
    }

    if daemon_available {
        println!();
        println!("{}", style("Delegation tools enabled (daemon running)").green().dim());
    } else {
        println!();
        println!("{}", style("Delegation tools disabled (start daemon to enable)").yellow().dim());
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
    println!("{}", style("Daemon Commands:").cyan().bold());
    println!("{}", style("─".repeat(40)).dim());
    println!("  {} - Manual delegation to daemon", style("/delegate <task>").yellow());
    println!("  {}    - Check daemon status", style("/daemon").yellow());
    println!();
    println!("{}", style("Automatic Delegation:").cyan().bold());
    println!("{}", style("─".repeat(40)).dim());
    println!("  When the daemon pool is running, the AI automatically");
    println!("  gains access to delegation tools:");
    println!("    {} - Delegate tasks to background workers", style("delegate_to_daemon").yellow());
    println!("    {}  - Check daemon pool status", style("check_daemon_status").yellow());
    println!();
    println!("  The AI will intelligently decide when to delegate based on:");
    println!("    • Long-running operations (builds, tests)");
    println!("    • Parallel independent tasks");
    println!("    • Background work that doesn't need immediate results");
    println!();
    println!("  Run {} to see if delegation tools are enabled.", style("/tools").yellow());
    println!();
}
