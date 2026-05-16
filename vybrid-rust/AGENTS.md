# Vybrid - Agent Guide

This guide helps agents work effectively in the Vybrid Rust codebase. Vybrid is an **expert Rust coding assistant** with deep knowledge of the Rust language and ecosystem.

## Project Overview

**Vybrid** is an AI-powered coding assistant built with Rust that provides an interactive CLI with tool-calling capabilities. It uses the **`openai/gpt-oss-120b`** model on [Groq’s OpenAI-compatible API](https://console.groq.com/docs/openai) and supports file operations, shell commands, code search, and project management.

**As an expert Rust coding agent**, Vybrid provides high-level Rust development assistance including code architecture, ownership patterns, async workflows, and ecosystem best practices.

**Key characteristics:**
- Built with Rust 2021 edition
- Async runtime: tokio
- Error handling: anyhow
- AI model: `openai/gpt-oss-120b` (Groq)
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

### Installation (Run from any directory)
```bash
./install.sh  # Installs vybrid to ~/.local/bin and updates PATH
source ~/.bashrc  # Apply changes to current session
```

After installation, you can run `vybrid` from any directory.

### Manual Installation
If the install script doesn't work:
```bash
cargo build --release
mkdir -p ~/.local/bin
cp target/release/vybrid ~/.local/bin/vybrid
chmod +x ~/.local/bin/vybrid
echo 'export PATH="$HOME/.local/bin:$PATH"' >> ~/.bashrc
source ~/.bashrc
```

### Running
```bash
vybrid  # From any directory after installation
cargo run  # Or run directly from project directory
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

### Agent tool: `run_cargo` (AI function calling)

Vybrid exposes a **`run_cargo`** tool for structured Cargo invocations (argv only; no shell injection). Prefer it over ad-hoc `execute_bash_command` when running `cargo check`, `build`, `test`, `clippy`, `fmt`, etc., on user projects. Use `diagnostic_format: "json"` when compiler-aware rustc/clippy summaries are more useful than raw Cargo output.

Canonical **Cargo quick reference**, **compile/fix loop**, **common diagnostics**, and **review heuristics** text for the system prompt lives in **[`src/rust_agent_reference.rs`](src/rust_agent_reference.rs)**. Implementation: **[`src/tools/cargo.rs`](src/tools/cargo.rs)** (timeouts, output cap, `--color never`, optional JSON diagnostic summaries).

## Project Structure

```
vybrid-rust/
├── src/
│   ├── main.rs              # CLI entry point, agent loop
│   ├── rust_agent_reference.rs  # Cargo/diagnostics text for system prompt (const strings)
│   ├── config.rs            # Configuration loading
│   ├── conversation.rs      # Conversation history management
│   ├── project_docs.rs      # Project documentation management
│   ├── ui.rs                # Terminal UI and formatting
│   ├── client/
│   │   ├── mod.rs           # Client module
│   │   └── groq.rs          # Groq OpenAI-compatible chat client, streaming, types
│   ├── tools/
│   │   ├── mod.rs           # Tools module
│   │   ├── definitions.rs   # Tool definitions for AI
│   │   ├── executor.rs      # Tool execution dispatch
│   │   ├── cargo.rs         # Structured `cargo` subprocess (run_cargo tool)
│   │   ├── rust.rs          # Rust diagnostics, cargo metadata, project snapshot tools
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

### Environment Variables
Use **`/menu`** or create env files manually. Saves go to **`~/.vybrid/.env`** and **`vybrid-rust/.env`** (same keys; kept in sync) so Vybrid finds keys when launched from any directory.

```bash
GROQ_API_KEY=your_api_key_here       # Required for AI chat — Groq API key
# GROQ_MODEL=openai/gpt-oss-120b     # Optional — defaults to openai/gpt-oss-120b

# Optional
SERPAPI_KEY=your_serpapi_key_here    # Optional — Google search (google_search tool)
```

### Configuration Loading
1. **`~/.vybrid/.env`** (if present)
2. **`vybrid-rust/.env`** — `CARGO_MANIFEST_DIR/.env` unless **`VYBRID_ROOT`** points at `vybrid-rust` (later file overrides duplicate keys)
3. Shell environment variables

### Directory Structure Created by Vybrid
```
vybrid-rust/
└── .env              # API keys (mirror) — gitignored

~/.vybrid/
├── .env              # API keys (mirror) — not in repo
├── messages/         # Message storage
└── progress/         # Progress tracking

~/.local/bin/
└── vybrid           # Installed binary (if using install.sh)
```

### Installing Vybrid for System-Wide Use

To run `vybrid` from any directory (not just the project directory):

```bash
cd /path/to/vybrid-rust
./install.sh
source ~/.bashrc
```

This will:
1. Build the release binary
2. Install it to `~/.local/bin/vybrid`
3. Add `~/.local/bin` to your PATH
4. Create `~/.vybrid/` for runtime data (messages, progress, API key mirror)

Keys are **`~/.vybrid/.env`** plus **`vybrid-rust/.env`** (same content). Launches from any cwd use `~/.vybrid/.env` if the project path is unavailable; optional **`VYBRID_ROOT`** still applies to the project `.env` path.

### Setting Up API Keys

```bash
vybrid   # from any directory after first /menu; or: ~/.vybrid/.env + vybrid-rust/.env
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

Example from `client/groq.rs`:
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
- `run_cargo` - Run Cargo via argv (`check`, `build`, `test`, `clippy`, etc.); preferred for Rust builds/tests; supports `diagnostic_format: "json"` for structured rustc/clippy summaries (see `src/tools/cargo.rs`)
- `cargo_metadata` - Return Cargo metadata JSON for workspace/package discovery
- `rust_project_snapshot` - Summarize edition, targets, features, dependencies, and workspace shape
- `explain_rust_diagnostic` - Explain rustc error codes and Rust topics such as ownership, traits, enums, lifetimes, and async `Send`

### Search
- `enhanced_grep` - Search files with regex, context lines, case sensitivity
- `google_search` - Google search via SerpAPI (requires SERPAPI_KEY)
- `list_memory_topics` - List available memory topic names without reading contents
- `read_memory_topic` - Read one project memory topic on demand
- `search_memory_transcripts` - Search raw transcripts for a specific identifier, path, symbol, or error code

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

### Tool Result Efficiency
- `read_file` and `read_multiple_files` use a session metadata cache through `ToolRuntime`. If the file length and modification time match the previous read, the cached content is reused and the tool header reports `cache: hit`.
- Oversized successful tool outputs are written to `~/.vybrid/progress/tool-results/` (or the configured progress dir). The model receives a preview plus a `Full result` path that can be read with `read_file` when exact omitted content is needed.

## API Integration

### Groq OpenAI-compatible Chat Completions (`openai/gpt-oss-120b`)
- Base URL: `https://api.groq.com/openai/v1` ([OpenAI compatibility](https://console.groq.com/docs/openai))
- Endpoint: `/chat/completions`
- Headers:
  - `Authorization: Bearer <GROQ_API_KEY>`
  - `Content-Type: application/json`
  - `Accept: text/event-stream` (for streaming)
- Default model: `openai/gpt-oss-120b` (optional override: env `GROQ_MODEL`)
- Max tokens: 8192 (within model completion limit; see [model docs](https://console.groq.com/docs/model/openai/gpt-oss-120b))
- Temperature: 1.0
- Request body: OpenAI-compatible (`model`, `messages`, `tools`, `tool_choice`, `stream`, `max_tokens`, `temperature`) — do not send unsupported fields ([compatibility](https://console.groq.com/docs/openai))
- Delta may include `reasoning_content` on some models; Groq may omit it

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
- Unit and integration tests exist for Cargo diagnostics, config env merging, context behavior, file edits, grep limits, and Rust helper tools.
- Rust-agent eval scenarios live in `evals/rust_agent_scenarios.md`.
- When adding tests:
  - Put unit tests in same module with `#[cfg(test)]`
  - Put integration tests in `tests/` directory
  - Mock API calls using `mockito` or similar for Groq client tests
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

## Project Documentation Feature

The `/docs` command allows you to add project-specific context documentation that the AI agent will automatically use when answering questions. This is particularly useful for:

- Framework-specific documentation (e.g., Fyrox, Bevy, Lumol)
- Project conventions and coding standards
- Architecture notes and design decisions
- API references specific to your project

### Usage

```
/docs                    - Show current project docs
/docs add <file>         - Add docs from a file
/docs read               - Enter interactive mode to add docs
/docs clear              - Clear all project docs
```

### Storage

Project documentation is stored in `.vybrid/docs.md` in the current project directory. This means each project can have its own documentation context.

### Automatic Context Injection

When you send a message to the AI, project docs are automatically appended to your message with a `---` separator. The AI will use this context when generating responses.

## Three-Layer Memory Feature

Vybrid uses a skeptical three-layer memory system to keep active context small:

1. **Core index**: `.vybrid/memory/MEMORY.md` is loaded into each user turn as a compact pointer list. Each non-empty line is capped at 150 characters.
2. **Topic files**: `.vybrid/memory/topics/<topic>.md` stores detailed knowledge and is fetched only through `read_memory_topic` when the index suggests it is relevant.
3. **Raw transcripts**: Session messages are appended under `~/.vybrid/messages/<project-key>/<session>.jsonl` and are searchable only through `search_memory_transcripts` for specific identifiers.

Memory is intentionally non-authoritative. Before relying on remembered paths, symbols, commands, dependencies, or behavior, verify against live project state with `read_file`, `enhanced_grep`, `rust_project_snapshot`, `cargo_metadata`, compiler output, or another direct tool.

### autoDream Consolidation

When a CLI session ends, Vybrid marks that session complete and may run autoDream consolidation. Three gates must pass before it writes anything: at least 24 hours since the previous run, at least 5 completed sessions since that run, and an exclusive `.vybrid/memory/autodream.lock` must be acquired.

autoDream runs four bounded phases:

1. **Orient**: scan `.vybrid/memory` for existing index/topic size.
2. **Gather**: extract compact signals from raw transcripts for completed sessions.
3. **Consolidate**: write `.vybrid/memory/topics/autodream.md` and add a pointer to `MEMORY.md`.
4. **Prune**: keep generated memory within the 200-line and 25KB budget.

The consolidation pass reads project memory and transcripts, and only writes memory artifacts. It does not resume old conversations or inject raw transcript content into active context.

### Example: Adding Bevy Framework Documentation

```bash
/docs read
> Project uses Bevy game engine (version 0.14)
> ECS architecture: Systems operate on Queries over Components
> Components are data structs with derive(Component)
> Resources are global data shared across systems
> Use #[derive(Resource)] for resources
> Schedule systems using App::add_systems()
> (empty line to finish)
```

### Example: Adding Documentation from a File

```bash
/docs add framework_docs.md
```

This makes all documentation in `framework_docs.md` available to the AI assistant.

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
2. Test coverage is focused on Rust tooling and core helpers; expand eval coverage as new agent behaviors are added
3. No CI/CD configuration
4. Shell commands have wall-clock timeouts and output caps; long-running interactive workflows should still be monitored by the user
5. No rate limiting for API calls
6. Raw transcripts are persisted for bounded search, but conversations do not resume from prior sessions
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

### `client/groq.rs` - API Client
- Handles communication with the Groq OpenAI-compatible chat completions API
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
cat vybrid-rust/.env   # API keys (or $VYBRID_ROOT/.env)
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
**AI Model**: `openai/gpt-oss-120b` (Groq)
