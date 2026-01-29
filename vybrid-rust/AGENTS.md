# Vybrid - Agent Guide

This guide helps agents work effectively in the Vybrid Rust codebase. Vybrid is an **expert Rust coding assistant** with deep knowledge of the Rust language and ecosystem.

## Project Overview

**Vybrid** is an AI-powered coding assistant built with Rust that provides an interactive CLI with tool-calling capabilities. It uses the GLM-4.7 API from Z.AI and supports file operations, shell commands, code search, and project management.

**As an expert Rust coding agent**, Vybrid provides high-level Rust development assistance including code architecture, ownership patterns, async workflows, and ecosystem best practices.

**Key characteristics:**
- Built with Rust 2021 edition
- Async runtime: tokio
- Error handling: anyhow
- AI model: GLM-4.7 (Z.AI API)
- Interactive CLI with streaming responses and tool calls

**Agent Capabilities:**
**Vybrid is an EXPERT RUST CODING AGENT** with deep mastery of:
- Rust ownership model, borrowing rules, and lifetime annotations
- Async/await patterns with tokio, futures, and async-stream
- Trait systems, generics, and type-level programming
- Error handling with anyhow::Context and thiserror
- Rust ecosystem: serde, reqwest, tokio, and common crates
- Performance optimization, zero-cost abstractions, and unsafe code patterns
- Idiomatic Rust code, iterators, Option/Result combinators, and pattern matching

## Essential Commands

### Building
```bash
cargo build
cargo build --release  # Optimized release build (LTO enabled, stripped)
```

### Running
```bash
cargo run
```

### Development
```bash
cargo check           # Quick compile check without building
cargo clippy          # Lint checks (if available)
cargo fmt             # Format code
```

### Testing
```bash
cargo test            # Run all tests
cargo test -- --nocapture  # Show test output
```

## Project Structure

```
vybrid-rust/
├── src/
│   ├── main.rs              # CLI entry point, agent loop
│   ├── config.rs            # Configuration loading
│   ├── conversation.rs      # Conversation history management
│   ├── ui.rs                # Terminal UI and formatting
│   ├── client/
│   │   ├── mod.rs           # Client module
│   │   └── glm.rs           # GLM-4.7 API client, streaming, types
│   ├── tools/
│   │   ├── mod.rs           # Tools module
│   │   ├── definitions.rs   # Tool definitions for AI
│   │   ├── executor.rs      # Tool execution dispatch
│   │   ├── file_ops.rs      # File operations (read, write, edit)
│   │   ├── grep.rs          # Code search with regex
│   │   ├── search.rs         # Google search via SerpAPI
│   │   ├── shell.rs         # Bash command execution
│   │   └── project.rs       # Project structure management
│   └── shell/
│       ├── mod.rs           # Shell module
│       └── persistent.rs    # Persistent shell mode
├── Cargo.toml               # Dependencies and build config
├── .env.example             # Environment variable template
└── .gitignore               # Git ignore patterns
```

## Configuration

### Environment Variables (Required)
Create `~/.vybrid/.env` or a local `.env` file:

```bash
ZAI_API_KEY=your_api_key_here        # Required - Z.AI API key
GLM_API_KEY=alternative_key_here     # Alternative - falls back to this if ZAI_API_KEY missing

# Optional
SERPAPI_KEY=your_serpapi_key_here    # Optional - For Google search functionality
```

### Configuration Loading Priority
1. Global config: `~/.vybrid/.env` (if exists)
2. Local config: `.env` in current directory (overrides global)
3. Environment variables (directly set)

### Directory Structure Created by Vybrid
```
~/.vybrid/
├── .env              # Global API keys
├── messages/         # Message storage
└── progress/         # Progress tracking
```

## Code Conventions

### Error Handling
- Use `anyhow::Result<T>` for most functions
- Use `anyhow::anyhow!("...")` or `anyhow::bail!("...")` for errors
- Use `.context("...")` from anyhow for error context
- Avoid `unwrap()` in production code

Example:
```rust
use anyhow::{Result, Context};

pub fn read_file(path: &str) -> Result<String> {
    let content = fs::read_to_string(path)
        .context(format!("Failed to read '{}'", path))?;
    Ok(content)
}
```

### Async/Await
- Use `tokio` as the async runtime
- Main function: `#[tokio::main] async fn main() -> Result<()>`
- All async functions return `Result<T>`
- Use `.await` for async calls

Example:
```rust
#[tokio::main]
async fn main() -> Result<()> {
    let result = some_async_function().await?;
    Ok(())
}
```

### Module Organization
- Each subdirectory has a `mod.rs` file
- Use `pub` for items that need to be visible outside module
- Re-export commonly used items with `pub use`
- Put `#![allow(dead_code)]` at top of modules with unused code during development

### File Paths
- Always normalize paths using `tools::file_ops::normalize_path()`
- Expand `~` to home directory
- Convert relative paths to absolute paths
- Use `std::path::PathBuf` for path manipulation

Example:
```rust
use crate::tools::file_ops::normalize_path;

let normalized = normalize_path(path);
```

### Tool Implementation Pattern
1. Define tool in `tools/definitions.rs` with name, description, and parameters
2. Implement logic in a file like `tools/shell.rs`, `tools/file_ops.rs`, etc.
3. Add dispatch in `tools/executor.rs::execute_tool()`
4. Import and use in `tools/executor.rs`

Example tool definition:
```rust
Tool {
    tool_type: "function".to_string(),
    function: FunctionDef {
        name: "my_tool".to_string(),
        description: "Tool description".to_string(),
        parameters: json!({
            "type": "object",
            "properties": {
                "param1": { "type": "string" }
            },
            "required": ["param1"]
        }),
    },
},
```

### Streaming Response Handling
- Use `futures::StreamExt` for streaming
- Accumulate partial results (e.g., tool_calls, content)
- Use `futures::pin_mut!()` to pin streams
- Handle SSE (Server-Sent Events) format with `"data: "` prefix and `"\n\n"` separator

Example from `client/glm.rs`:
```rust
futures::pin_mut!(stream);
while let Some(chunk_result) = stream.next().await {
    match chunk_result {
        Ok(chunk) => {
            // Process chunk
        }
        Err(e) => return Err(e),
    }
}
```

### UI and Output Formatting
- Use `console::style()` for colored output
- Color scheme:
  - Errors: `style().red()`
  - Success: `style().green()`
  - Info: `style().dim()`
  - Commands: `style().yellow()`
  - User prompts: `style().magenta().bold()`
  - Assistant output: `style().cyan().bold()`
- Use `print!()` and `eprintln!()` for output (no logging framework)
- Always flush stdout after prompts: `io::stdout().flush()?`

## Available Tools (for AI Agent)

The system provides these tools to the AI:

### File Operations
- `read_file` - Read single file
- `read_multiple_files` - Read multiple files
- `create_file` - Create or overwrite file
- `create_multiple_files` - Create multiple files
- `edit_file` - Replace exact snippet in file (requires exact match)

### Shell/Execution
- `execute_bash_command` - Run shell command (supports working_directory)

### Search
- `enhanced_grep` - Search files with regex, context lines, case sensitivity
- `google_search` - Google search via SerpAPI (requires SERPAPI_KEY)

### Project Management
- `create_project_structure` - Create mandatory project files
- `get_current_todo_items` - Read incomplete tasks
- `mark_todo_complete` - Mark task as complete

## Important Gotchas

### Tool Parameter Parsing
- Tool arguments come as JSON strings in `tools::executor.rs`
- Use `serde_json::Value` to parse arguments
- Use `.as_str()`, `.as_bool()`, `.as_u64()` for extraction
- Provide defaults: `.unwrap_or(...)` or `.unwrap_or_default()`

### File Edit Exact Matching
- `edit_file` requires **exact** string match including whitespace
- No fuzzy matching - if snippet appears multiple times, editing fails
- Tool validates uniqueness before editing
- Consider adding more context to make match unique

### Shell Command Execution
- Commands run via `bash -c "command"`
- Stdout and stderr are captured and returned
- Exit codes included in output
- Persistent shell mode maintains state across commands
- `cd` commands in tool calls affect only that command (not persistent)

### Streaming Tool Calls
- Tool calls arrive incrementally during streaming
- Must accumulate partial results (name chunks, argument chunks)
- Index used to track which tool call updates apply to
- Execute tools after streaming completes

### Dead Code Warnings
- Many modules have `#![allow(dead_code)]` at top
- This is intentional during development
- Remove these attributes before production releases

### Conversation Management
- System prompt is first message, never cleared
- User messages added with `conversation.add_user_message()`
- Assistant messages added with `conversation.add_assistant_message()`
- Tool results added with `conversation.add_tool_result()`
- `conversation.clear_keeping_system()` clears everything except system prompt

## API Integration

### GLM-4.7 API Details
- Base URL: `https://api.z.ai/api/coding/paas/v4`
- Endpoint: `/chat/completions`
- Headers:
  - `Authorization: Bearer <api_key>`
  - `Content-Type: application/json`
  - `Accept: text/event-stream` (for streaming)
- Model: `glm-4.7`
- Max tokens: 8192
- Temperature: 0.7
- Thinking mode: Enabled (provides reasoning_content in delta)

### Streaming Response Format
```
data: {"id":"...","choices":[{"index":0,"delta":{"role":"assistant","content":"..."}}],"usage":{...}}

data: {"id":"...","choices":[{"index":0,"delta":{"reasoning_content":"..."}}]}

data: [DONE]
```

## Development Workflow

### Adding a New Tool
1. Define tool schema in `src/tools/definitions.rs::get_all_tools()`
2. Implement logic in appropriate file (e.g., `src/tools/mytool.rs`)
3. Add match case in `src/tools/executor.rs::execute_tool()`
4. Import in `src/tools/mod.rs` if new file
5. Test by running Vybrid and calling the tool through AI

### Adding a New Module
1. Create new file in `src/` or subdirectory
2. Add `mod filename;` to parent's `mod.rs` or `main.rs`
3. Add `pub use filename::important_item;` for re-exports
4. Add to `.gitignore` if needed (e.g., generated files)

### Debugging Tips
- Use `eprintln!()` for debug output (goes to stderr)
- Check environment variables with `std::env::var()`
- Use `anyhow::Context` to add helpful error messages
- Test tools by calling them directly in executor.rs
- Use `RUST_LOG=debug` if logging is added (no logging currently configured)

### Testing Strategy
- **Note**: No test files currently exist in the project
- When adding tests:
  - Put unit tests in same module with `#[cfg(test)]`
  - Put integration tests in `tests/` directory
  - Mock API calls using `mockito` or similar for GLM client tests
  - Test tool execution paths in executor.rs

## Dependencies Key Points

### Critical Dependencies
- `tokio` (v1, features: full, signal, process) - Async runtime
- `reqwest` (v0.12, features: json, stream) - HTTP client
- `serde`/`serde_json` (v1) - Serialization
- `anyhow` (v1) - Error handling
- `regex` (v1) - Regex for grep functionality
- `glob` (v0.3) - File pattern matching
- `dirs` (v5) - Directory paths (home, etc.)
- `chrono` (v0.4) - Date/time handling
- `futures`, `tokio-stream`, `async-stream` - Async utilities

### CLI Dependencies
- `dialoguer` (v0.11) - Interactive prompts
- `console` (v0.15) - Terminal styling

### Control Flow
- `ctrlc` (v3.4) - Ctrl+C signal handling

## Release Configuration

The `Cargo.toml` release profile is optimized:
```toml
[profile.release]
opt-level = 3        # Maximum optimization
lto = true          # Link-time optimization
strip = true        # Remove debug symbols
```

Build release binary with:
```bash
cargo build --release
```

The binary will be at `target/release/vybrid`.

## Shell Mode

Two shell modes available:

### 1. Single Command Mode
Prefix command with `!`:
```
!ls -la
```

### 2. Persistent Shell Mode
Type `!` alone to enter interactive shell:
```
!
> cd /path/to/dir
> pwd
> ls
> (empty line to exit)
```

- Directory changes persist within shell session
- State maintained across commands
- Type empty line or `exit` to return to Vybrid

## Project Files Created by Vybrid

When using `create_project_structure` tool:

1. **tasks/todo.md** - Task tracking with checkboxes
2. **docs/activity.md** - Activity log with timestamps
3. **docs/PROJECT_README.md** - Project context for AI agents

These files follow the Vybrid development methodology and are automatically generated or updated.

## System Prompt Integration

The system prompt (in `main.rs::get_system_prompt()`) defines Vybrid as an **EXPERT RUST CODING AGENT** with:

**Rust Expertise:**
- Deep knowledge of Rust ownership model, borrowing rules, and lifetime annotations
- Proficiency with async Rust using tokio, futures, and async-stream
- Mastery of Rust trait system, generics, and type-level programming
- Expert in error handling: Result<T>, anyhow::Context, thiserror for custom errors
- Familiarity with Rust ecosystem: serde, reqwest, tokio, and common crates
- Experience with performance optimization, zero-cost abstractions, and unsafe code when needed

**Workflow Requirements:**
- Mandatory project structure files
- Continuous development workflow
- Task tracking with checkboxes
- Activity logging with timestamps
- Immediate execution of tasks without waiting for approval

Agents working in Vybrid should respect both the workflow pattern and leverage the Rust expertise capabilities.

## Common Patterns

### File Path Normalization
```rust
let normalized = tools::file_ops::normalize_path(path);
```

### Error with Context
```rust
let result = some_operation()
    .context("Failed to do X")?;
```

### Async Stream Processing
```rust
futures::pin_mut!(stream);
while let Some(result) = stream.next().await {
    // process
}
```

### JSON Argument Parsing
```rust
let args: Value = serde_json::from_str(arguments)
    .unwrap_or(Value::Object(serde_json::Map::new()));
let param = args["param"].as_str().unwrap_or("");
```

### Tool Execution
```rust
tools::executor::execute_tool(&tool_name, &arguments).await
```

## Security Notes

- API keys are loaded from environment variables (not hardcoded)
- `.env` file is in `.gitignore`
- Commands run in subprocess (no shell injection risk from user input)
- File operations respect filesystem permissions
- No authentication/authorization system - assumes trusted environment

## Known Limitations

1. No built-in logging system (uses println!/eprintln!)
2. No test suite currently exists
3. No CI/CD configuration
4. Shell commands run synchronously (no timeout handling)
5. No rate limiting for API calls
6. No conversation persistence across sessions (in-memory only)
7. No syntax highlighting for code output
8. No file watching/reloading

## When to Use Different Entry Points

### `main.rs` - CLI Entry Point
- Entry point for the interactive Vybrid CLI
- Parses configuration and displays banner
- Runs the agent mode loop

### `tools/executor.rs` - Tool Dispatch
- Called by main.rs when AI requests tool execution
- Parses tool arguments and routes to implementation
- Returns results to be added to conversation

### `client/glm.rs` - API Client
- Handles communication with GLM-4.7 API
- Provides streaming and non-streaming methods
- Manages request/response parsing

## Contributing Workflow

When making changes:
1. Read the existing code carefully to understand patterns
2. Follow existing error handling conventions (anyhow)
3. Use `#![allow(dead_code)]` only temporarily during development
4. Test by running `cargo run` and using the CLI
5. Ensure environment variables are set before running
6. Verify tool execution through actual AI conversation

## Quick Reference

### Build and Run
```bash
cargo build && cargo run
```

### Check Configuration
```bash
cat ~/.vybrid/.env  # or local .env
```

### Test a Tool Directly (in code)
```rust
let result = tools::executor::execute_tool("read_file", r#"{"file_path":"Cargo.toml"}"#).await?;
println!("{}", result);
```

### Add Debug Output
```rust
eprintln!("Debug: {:?}", some_variable);
```

---

**Last Updated**: January 28, 2026
**Rust Edition**: 2021
**AI Model**: GLM-4.7
