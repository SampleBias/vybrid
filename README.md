# Vybrid

AI powered coding assistant built from the trenches with Rust to save humanity from bloat.

## Features

- **Agent Mode**: Full AI Engineer with file operations, shell commands, and web search
- **Daemon Mode**: Background service for processing execution requests
- **Persistent Shell**: Interactive shell mode with state persistence
- **Function Calling**: 10+ tools for file operations, code search, and more

## Requirements

- Linux (Ubuntu 20.04+ recommended)
- Rust 1.70+ (for building from source)
- Z.AI API key for GLM-5.1 (GLM Coding Plan)

## Installation

### From Source

```bash
# Clone or navigate to the project
cd vybrid-rust

# Build release version
cargo build --release

# Binary will be at target/release/vybrid
```

### Copy Binary (Optional)

```bash
# Copy to user bin
cp target/release/vybrid ~/.local/bin/

# Or system-wide
sudo cp target/release/vybrid /usr/local/bin/
```

## Configuration

Create a `.env` file in your working directory or set environment variables:

```bash
# Required: Z.AI API Key
ZAI_API_KEY=your_api_key_here

# Optional: SerpAPI for Google Search
SERPAPI_KEY=your_serpapi_key_here
```

Get your API key from [Z.AI Open Platform](https://z.ai/model-api).

## Usage

```bash
# Run Vybrid
./vybrid

# Or if installed to PATH
vybrid
```

### Mode Selection

On startup, choose between:

1. **Agent Mode** - Full AI Engineer with all tools
2. **Daemon Mode** - Background service for processing requests

### Agent Mode Commands

| Command | Description |
|---------|-------------|
| `exit`, `quit` | Exit Vybrid |
| `!` | Enter persistent shell mode |
| `!<cmd>` | Execute single shell command |
| `/add <path>` | Add file to conversation |
| `/pwd` | Show current directory |
| `/tools` | List available AI tools |
| `/new` | Start new conversation |
| `/help` | Show help |
| `clear` | Clear screen |

### Available Tools

The AI can use these tools automatically:

- **File Operations**: `read_file`, `read_multiple_files`, `create_file`, `create_multiple_files`, `edit_file`
- **Shell**: `execute_bash_command`
- **Search**: `enhanced_grep`, `google_search`
- **Project**: `create_project_structure`, `get_current_todo_items`, `mark_todo_complete`

## Project Structure

```
vybrid-rust/
├── Cargo.toml          # Dependencies and project config
├── src/
│   ├── main.rs         # Entry point
│   ├── config.rs       # Configuration management
│   ├── conversation.rs # Conversation history
│   ├── ui.rs           # Terminal UI helpers
│   ├── client/
│   │   └── glm.rs      # Z.AI chat completions client
│   ├── tools/
│   │   ├── definitions.rs  # Tool schemas
│   │   ├── executor.rs     # Tool dispatcher
│   │   ├── file_ops.rs     # File operations
│   │   ├── shell.rs        # Shell execution
│   │   ├── grep.rs         # Code search
│   │   ├── search.rs       # Google search
│   │   └── project.rs      # Project structure
│   ├── daemon/
│   │   ├── pool.rs     # Worker pool
│   │   ├── queue.rs    # Message queue
│   │   └── worker.rs   # Request processor
│   └── shell/
│       └── persistent.rs   # Persistent shell
```

## Data Directory

Vybrid stores data in `~/.vybrid/`:

- `messages/` - Inter-process communication for daemon mode
- `daemon_pool/` - Daemon lock files
- `progress/` - Request progress tracking

## License

MIT License

## Credits

Powered by [GLM-5.1](https://docs.z.ai/devpack/using5.1) on the [GLM Coding Plan](https://docs.z.ai/devpack/overview) from Z.AI (Zhipu AI).
