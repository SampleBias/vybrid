<p align="center">
  <img src="assets/vybrid-banner.png" alt="Vybrid — AI Coding Assistant from the Trenches built in Rust" width="800" />
</p>

# Vybrid

AI powered coding assistant built from the trenches with Rust to save humanity from bloat.

**Repository:** [github.com/SampleBias/vybrid](https://github.com/SampleBias/vybrid)

## Features

- **Agent Mode**: Full AI Engineer with file operations, shell commands, and web search
- **Daemon Mode**: Background service for processing execution requests
- **Persistent Shell**: Interactive shell mode with state persistence
- **Function Calling**: 10+ tools for file operations, code search, and more

## Requirements

- Linux (Ubuntu 20.04+ recommended)
- Rust 1.70+ (for building from source)
- [Groq](https://console.groq.com/) API key ([`openai/gpt-oss-120b`](https://console.groq.com/docs/model/openai/gpt-oss-120b))

## Installation

### From Source

```bash
git clone https://github.com/SampleBias/vybrid.git
cd vybrid/vybrid-rust

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

Keys are loaded from **`~/.vybrid/.env`** first, then **`vybrid-rust/.env`** (project wins if both define the same variable). When you save via **`/menu`**, both files are updated so you only configure once and can run Vybrid from **any directory**. Use **`/menu`** or create the files manually:

```bash
# Required: Groq API key
GROQ_API_KEY=your_api_key_here

# Optional: override model (default is openai/gpt-oss-120b)
# GROQ_MODEL=openai/gpt-oss-120b

# Optional: SerpAPI for Google Search
SERPAPI_KEY=your_serpapi_key_here
```

If you run a compiled binary from a different path, set **`VYBRID_ROOT`** to the `vybrid-rust` directory so Vybrid can find `.env`.

Create a key in the [Groq Console](https://console.groq.com/keys).

Keep real keys out of version control: `.env` and `.env.*` are listed in the repo `.gitignore` (only `.env.example` is meant to be committed as a template).

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
| `/menu` | Menu (add keys; saved to `~/.vybrid/.env` and `vybrid-rust/.env`) |
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
│   │   └── groq.rs     # Groq OpenAI-compatible chat client
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

- **API keys**: mirrored in **`~/.vybrid/.env`** and **`vybrid-rust/.env`** (same values; use from any cwd)

Vybrid also stores runtime data in `~/.vybrid/`:

- `messages/` - Inter-process communication for daemon mode
- `daemon_pool/` - Daemon lock files
- `progress/` - Request progress tracking

## License

MIT License

## Credits

Inference via [Groq](https://groq.com/) using the [`openai/gpt-oss-120b`](https://console.groq.com/docs/model/openai/gpt-oss-120b) model ([OpenAI-compatible API](https://console.groq.com/docs/openai)).
