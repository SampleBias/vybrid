<p align="center">
  <img src="assets/vybrid-banner.png" alt="Vybrid — AI Coding Assistant from the Trenches built in Rust" width="800" />
</p>

# Vybrid

AI powered coding assistant built from the trenches with Rust to save humanity from bloat.

**Repository:** [github.com/SampleBias/vybrid](https://github.com/SampleBias/vybrid)

## Features

- **Multi-Provider LLM**: Groq (default), [OpenRouter](https://openrouter.ai/) (400+ cloud models), or local [LM Studio](https://lmstudio.ai/)
- **Agent Mode**: Full AI Engineer with file operations, shell commands, and web search
- **Agent Skills**: Pi-compatible [Agent Skills](https://agentskills.io/specification) — on-demand `SKILL.md` workflows discovered from `~/.vybrid/skills/`, `.vybrid/skills/`, and shared `~/.agents/skills/` paths
- **Context Compaction**: `/compact` summarizes older conversation history via the LLM to free context; passive overflow compaction still applies automatically during long sessions
- **Three-Layer Memory**: Compact `MEMORY.md` index, on-demand topic files, and searchable raw transcripts with optional autoDream consolidation
- **Project Index**: `index.md` navigation via `/index` and `read_project_index` / `generate_project_index` tools
- **Project Docs**: Persistent `.vybrid/docs.md` context via `/docs` commands
- **Thinking Levels**: `/thinking` to show or set GPT-OSS reasoning effort (`low` / `medium` / `high`)
- **Rust-Aware Diagnostics**: Structured Cargo/rustc summaries for ownership, traits, enums, lifetimes, async, and clippy feedback
- **Optional Rust LSP**: Connect to `rust-analyzer` for live diagnostics, hover, definitions, references, symbols, completions, code actions, and formatting edits
- **Persistent Shell**: Interactive shell mode with state persistence and filesystem path autocomplete
- **Function Calling**: Tools for file operations, code search, Cargo, rustc explanations, Rust project snapshots, memory, and optional LSP queries

## Requirements

- Linux (Ubuntu 20.04+ recommended)
- Rust 1.70+ (for building from source)
- Optional: [`rust-analyzer`](https://rust-analyzer.github.io/) for Rust LSP integration
- One of:
  - **[Groq](https://console.groq.com/)** API key ([`openai/gpt-oss-120b`](https://console.groq.com/docs/model/openai/gpt-oss-120b)), or
  - **[OpenRouter](https://openrouter.ai/)** API key (access to hundreds of models via one OpenAI-compatible API), or
  - **[LM Studio](https://lmstudio.ai/)** with the [local server](https://lmstudio.ai/docs/developer/core/server) running and a model loaded (OpenAI-compatible API — see below)

## Installation

### From Source

```bash
git clone https://github.com/SampleBias/vybrid.git
cd vybrid/vybrid-rust

# Build release version
cargo build --release

# Binary will be at target/release/vybrid
```

### Install on PATH (recommended)

From `vybrid-rust/`, use the install script so the `vybrid` command stays in sync with your checkout (avoids an old `~/.local/bin/vybrid` from a previous build):

```bash
chmod +x install.sh   # once
./install.sh
```

This builds `--release` and copies to `~/.local/bin/vybrid`. Override the destination with `INSTALL_DEST=/path/to/vybrid ./install.sh` if needed.

Ensure `~/.local/bin` comes **before** any other directory that might contain another `vybrid`:

```bash
export PATH="$HOME/.local/bin:$PATH"
```

After `git pull`, run `./install.sh` again.

### Copy Binary (manual)

```bash
cp target/release/vybrid ~/.local/bin/
# Or system-wide:
# sudo cp target/release/vybrid /usr/local/bin/
```

## Configuration

Keys are loaded from **`~/.vybrid/.env`** first, then **`vybrid-rust/.env`** (project wins if both define the same variable). When you save via **`/menu`**, both files are updated so you only configure once and can run Vybrid from **any directory**. Use **`/menu`** or create the files manually:

```bash
# LLM backend: groq (default), openrouter, or lmstudio
# VYBRID_LLM_PROVIDER=groq

# --- Groq (cloud) ---
GROQ_API_KEY=your_api_key_here
# Optional: override model (default is openai/gpt-oss-120b)
# GROQ_MODEL=openai/gpt-oss-120b
# Optional: current-response fallback after two OSS120B rate-limit waits
# GROQ_RATE_LIMIT_FALLBACK_MODEL=qwen/qwen3-32b
# Optional: sampling temperature 0.0–2.0 (default 0.3; low improves tool-call reliability)
# VYBRID_TEMPERATURE=0.3
# Optional: GPT-OSS reasoning effort low|medium|high (unset = provider default, medium).
# `low` is noticeably faster/cheaper for tool-heavy agent loops.
# VYBRID_REASONING_EFFORT=low
# Optional: Groq service tier auto|on_demand|flex|performance (default auto)
# VYBRID_GROQ_SERVICE_TIER=auto

# --- OpenRouter (multi-provider cloud) — set VYBRID_LLM_PROVIDER=openrouter ---
# OPENROUTER_API_KEY=your_openrouter_api_key_here
# OPENROUTER_MODEL=openai/gpt-4o-mini

# --- LM Studio (local) — set VYBRID_LLM_PROVIDER=lmstudio and fill these ---
# LM_STUDIO_BASE_URL=http://127.0.0.1:1234/v1
# LM_STUDIO_API_KEY=lm-studio
# LM_STUDIO_MODEL=your-loaded-model-id

# Optional: SerpAPI for Google Search
SERPAPI_KEY=your_serpapi_key_here

# Optional: Rust LSP (rust-analyzer)
# VYBRID_RUST_LSP_ENABLED=false
# VYBRID_RUST_LSP_COMMAND=rust-analyzer
# VYBRID_RUST_LSP_ROOT=/path/to/workspace
```

### Active provider (`VYBRID_LLM_PROVIDER`) and coexisting credentials

- **`VYBRID_LLM_PROVIDER`** is `groq` (default), `openrouter`, or `lmstudio`. It selects **which backend handles chat**; only one runs at a time.
- You can keep **Groq**, **OpenRouter**, and **LM Studio** settings in the same `~/.vybrid/.env` and `vybrid-rust/.env`. They do not overwrite each other. Switching the active provider (via **`/menu`** or editing **`VYBRID_LLM_PROVIDER`**) only changes **which** profile is used, not the stored keys for the other backends.

If you run a compiled binary from a different path, set **`VYBRID_ROOT`** to the `vybrid-rust` directory so Vybrid can find `.env`.

Create a Groq key in the [Groq Console](https://console.groq.com/keys). For OpenRouter, get a key at [openrouter.ai/keys](https://openrouter.ai/keys) and pick a model via **`/menu`** (see [OpenRouter (cloud, multi-provider)](#openrouter-cloud-multi-provider)). For LM Studio, use **`/menu`** → **LM Studio** or set the variables above; see [LM Studio (local, offline)](#lm-studio-local-offline).

### OpenRouter (cloud, multi-provider)

Vybrid talks to [OpenRouter](https://openrouter.ai/) through its **OpenAI-compatible** API at `https://openrouter.ai/api/v1/chat/completions` — the same chat flow as Groq and LM Studio, with access to hundreds of models from Anthropic, OpenAI, Google, Meta, DeepSeek, and others.

1. Create an API key at [openrouter.ai/keys](https://openrouter.ai/keys).
2. Run **`/menu`** → **OpenRouter (multi-provider models)** → **Add or update OpenRouter API key**.
3. Run **`/menu`** → **OpenRouter** → **Select model** and choose how to browse:
   - **Recommended for coding** — popular programming models that support tool calling (~30–50 items; best default)
   - **Search by name** — e.g. `claude`, `gpt-4`, `qwen`
   - **Browse by provider** — Anthropic, OpenAI, Google, etc.
   - **Advanced: full catalog** — complete OpenRouter list (models without tool support are marked `[no tools]`)
4. Run **`/menu`** → **OpenRouter** → **Switch to OpenRouter** (or set **`VYBRID_LLM_PROVIDER=openrouter`**).

Model lists are fetched live from OpenRouter and cached under `~/.vybrid/cache/` for six hours. Use **Refresh model catalog** in the OpenRouter submenu to force a refetch.

Manual `.env` example:

```bash
VYBRID_LLM_PROVIDER=openrouter
OPENROUTER_API_KEY=your_openrouter_api_key_here
OPENROUTER_MODEL=anthropic/claude-sonnet-4
```

Vybrid only sends tool calls to models that support the `tools` parameter. The recommended picker filters to tool-capable models automatically.

### LM Studio (local, offline)

Vybrid talks to LM Studio’s **OpenAI-compatible** HTTP API ([docs](https://lmstudio.ai/docs/developer/openai-compat)): same `POST /v1/chat/completions` flow as Groq, with base URL typically `http://127.0.0.1:1234/v1` (port is configurable in LM Studio under **Developer → Local Server**).

1. In LM Studio, load a model and start the local server ([server overview](https://lmstudio.ai/docs/developer/core/server)).
2. Set **`VYBRID_LLM_PROVIDER=lmstudio`**.
3. Set **`LM_STUDIO_MODEL`** to the **exact model identifier** of the loaded model (must match what LM Studio exposes for chat).
4. Set **`LM_STUDIO_BASE_URL`** if you are not using the default host/port (default in Vybrid is `http://127.0.0.1:1234/v1`).
5. **API key**: If the server has **Require authentication** off, you can use a placeholder (Vybrid defaults to `lm-studio`). If authentication is on (LM Studio 0.4.0+), create a token under **Developer → Server Settings → Manage Tokens** and set **`LM_STUDIO_API_KEY`**; requests use `Authorization: Bearer` as in the [authentication docs](https://lmstudio.ai/docs/developer/core/authentication).

Use **`/menu`** to save these to `~/.vybrid/.env` and `vybrid-rust/.env`. Use **`/menu`** → **Groq** → **Switch to Groq** when you want to return to Groq.

Keep real keys out of version control: `.env` and `.env.*` are listed in the repo `.gitignore` (only `.env.example` is meant to be committed as a template).

### Rust LSP (rust-analyzer)

Vybrid can optionally start and talk to **`rust-analyzer`** over the Language Server Protocol (LSP). This is separate from the LLM provider: Groq, OpenRouter, or LM Studio still handles chat, while `rust-analyzer` provides editor-grade Rust intelligence when enabled.

The LSP is **off by default**. If `VYBRID_RUST_LSP_ENABLED=true`, Vybrid tries to connect at startup. If `rust-analyzer` is missing or the command is wrong, Vybrid shows an error indicator but keeps the rest of the app working.

#### Install rust-analyzer

On Arch Linux:

```bash
sudo pacman -S --needed rust-analyzer
```

With rustup:

```bash
rustup component add rust-analyzer
```

Verify it is available:

```bash
command -v rust-analyzer
rust-analyzer --version
```

If `command -v rust-analyzer` prints nothing, Vybrid will not be able to connect until you install it or configure `VYBRID_RUST_LSP_COMMAND` with the full path to the binary.

#### Connect from /menu

1. Start Vybrid from your project workspace.
2. Run **`/menu`**.
3. Choose **“Rust LSP (rust-analyzer)”**.
4. Choose **“Connect now”**.
5. Confirm the prompt status line changes from `○ rust-lsp off` to `● rust-lsp connected`.

The Rust LSP submenu also supports:

- **Connect now**: Starts `rust-analyzer` for the configured workspace root.
- **Disconnect**: Shuts down the LSP process and clears cached diagnostics.
- **Restart**: Stops and reconnects the LSP.
- **Show status**: Prints the current state, command, workspace root, and any error message.
- **Enable/disable auto-connect**: Persists `VYBRID_RUST_LSP_ENABLED`.
- **Configure rust-analyzer command**: Persists `VYBRID_RUST_LSP_COMMAND`; use this if the binary is not on `PATH`.
- **Configure workspace root**: Persists `VYBRID_RUST_LSP_ROOT`; leave empty to use the current directory at runtime.

#### Status indicator

Vybrid shows the Rust LSP state in the main prompt status line, next to the context meter:

- `○ rust-lsp off` - LSP is disabled or disconnected.
- `◌ rust-lsp connecting` - Vybrid is starting and initializing `rust-analyzer`.
- `● rust-lsp connected` - LSP is ready and LSP-backed tools can be used.
- `× rust-lsp error` - Connection failed or the LSP reader encountered an error. Use `/menu` → “Rust LSP (rust-analyzer)” → “Show status” for details.

#### Rust LSP config keys

- **`VYBRID_RUST_LSP_ENABLED`**: `true` or `false`; controls startup auto-connect.
- **`VYBRID_RUST_LSP_COMMAND`**: Command used to spawn the server, usually `rust-analyzer`. You can set an absolute path if needed.
- **`VYBRID_RUST_LSP_ROOT`**: Optional workspace root. If empty or unset, Vybrid uses the current directory when it connects.

Example:

```bash
VYBRID_RUST_LSP_ENABLED=true
VYBRID_RUST_LSP_COMMAND=rust-analyzer
VYBRID_RUST_LSP_ROOT=/home/me/projects/my-rust-app
```

## Usage

```bash
# Run Vybrid
./vybrid

# Or if installed to PATH
vybrid
```

### Startup

Vybrid starts directly in **Agent Mode** with all tools available.

### Agent Mode Commands

Run **`/help`** in the REPL for the full list. Example:

<p align="center">
  <img src="assets/vybrid-help-menu.png" alt="Vybrid /help menu showing general, project docs, skills, and context commands" width="920" />
</p>

#### General

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
| `/menu` | Setup menu: Groq, OpenRouter, LM Studio, SerpAPI, Rust LSP (saved to `~/.vybrid/.env` and `vybrid-rust/.env`) |
| `/index` | Show, refresh, or print path for compact project `index.md` |
| `/thinking` | Show or set reasoning effort: `/thinking low\|medium\|high\|default` |
| `clear` | Clear screen |

#### Project docs

| Command | Description |
|---------|-------------|
| `/docs` | Show current project docs (`.vybrid/docs.md`) |
| `/docs add <file>` | Append docs from a file |
| `/docs read` | Enter docs interactively |
| `/docs clear` | Clear project docs |

#### Skills

| Command | Description |
|---------|-------------|
| `/skills` | List discovered skills |
| `/skills reload` | Rescan skill directories and refresh the system prompt |
| `/skills path` | Show skill search paths |
| `/skill:<name> [args]` | Load a skill into the conversation |
| `/skills load <name> [args]` | Same as `/skill:<name>` |

#### Context

| Command | Description |
|---------|-------------|
| `/compact [focus]` | Summarize older messages to free context (requires a configured LLM) |

### Agent Skills

Vybrid implements the [Agent Skills standard](https://agentskills.io/specification). Skills are directories with a `SKILL.md` file (YAML frontmatter + instructions). At startup, only **names and descriptions** are injected into the system prompt; the agent loads full instructions on demand with `read_file` or when you run `/skill:<name>`.

**Search paths** (first match wins on name collisions):

| Scope | Path |
|-------|------|
| Global | `~/.vybrid/skills/` |
| Global (shared) | `~/.agents/skills/` |
| Project | `.vybrid/skills/` under the project root |
| Project (shared) | `.agents/skills/` walking up from the current directory |

Copy the example skill from [`vybrid-rust/skills/rust-compile-fix-loop/`](vybrid-rust/skills/rust-compile-fix-loop/) into one of those directories, then run **`/skills reload`**.

Minimal `SKILL.md` shape:

```markdown
---
name: my-skill
description: What this skill does and when the agent should use it.
---

# Instructions

Steps, scripts, and references go here.
```

### Context compaction

Long agent sessions can fill the context window. Vybrid handles this in two ways:

1. **Automatic** — When a request exceeds the token budget, older messages are dropped from the API payload while keeping a stable prefix for Groq prompt caching.
2. **Manual** — Run **`/compact`** (optional focus text) to LLM-summarize older history in place, preserving the most recent messages. Example: `/compact preserve compiler errors and files still being edited`.

The status line shows estimated context usage before each prompt (`ctx` meter).

### Project memory

Vybrid keeps long-term hints in `.vybrid/memory/` without loading everything every turn:

- **`MEMORY.md`** — Compact index of pointers (injected lightly each turn)
- **`topics/<name>.md`** — Detailed topic files, read via `read_memory_topic` when relevant
- **Raw transcripts** — Stored under `~/.vybrid/messages/`; search with `search_memory_transcripts` for specific paths, symbols, or error codes

After enough completed sessions, **autoDream** may consolidate recent transcripts into `topics/autodream.md`. Treat all memory as hints — verify against live project tools before acting.

### `/menu` setup

The setup menu is organized by provider so long model lists stay manageable:

| Submenu | What it configures |
|---------|-------------------|
| **Groq** | API key, quick setup with optional SerpAPI, switch active provider |
| **OpenRouter** | API key, tiered model picker, switch provider, refresh model cache |
| **LM Studio** | Local server URL, API token, model id |
| **SerpAPI** | Google search key for the `google_search` tool |
| **Rust LSP** | `rust-analyzer` connect, auto-connect, workspace root |

OpenRouter’s model picker defaults to **Recommended for coding** (popular, tool-capable programming models). Use search, browse-by-provider, or the full catalog when you need something specific.

### Persistent Shell Mode

Type `!` to enter persistent shell mode. Directory changes persist while you are in shell mode, regular Bash commands like `ls`, `pwd`, and `cargo test` continue to work, and pressing <kbd>Tab</kbd> autocompletes filesystem paths.

### Available Tools

The AI can use these tools automatically:

- **File Operations**: `read_file`, `read_multiple_files`, `create_file`, `create_multiple_files`, `edit_file`
- **Shell**: `execute_bash_command`
- **Rust**: `run_cargo`, `cargo_metadata`, `rust_project_snapshot`, `explain_rust_diagnostic`
- **Rust LSP**: `rust_lsp_query` when `rust-analyzer` is connected
- **Search**: `enhanced_grep`, `google_search`
- **Memory**: `list_memory_topics`, `read_memory_topic`, `search_memory_transcripts`
- **Project index**: `read_project_index`, `generate_project_index`
- **Project**: `create_project_structure`, `get_current_todo_items`, `mark_todo_complete`

## Project Structure

```
vybrid-rust/
├── Cargo.toml          # Dependencies and project config
├── skills/             # Example Agent Skills (copy to ~/.vybrid/skills/ or .vybrid/skills/)
├── src/
│   ├── main.rs         # Entry point and slash commands
│   ├── menu.rs         # /menu setup (Groq, OpenRouter, LM Studio, SerpAPI, LSP)
│   ├── config.rs       # Configuration management
│   ├── conversation.rs # Conversation history, compaction, context pruning
│   ├── memory.rs       # Three-layer memory and autoDream consolidation
│   ├── skills.rs       # Agent Skills discovery and loading
│   ├── project_index.rs # index.md generation
│   ├── lsp.rs          # Optional rust-analyzer LSP client
│   ├── project_docs.rs # Project-specific docs context
│   ├── rust_agent_reference.rs # Rust workflow and diagnostic guidance
│   ├── ui.rs           # Terminal UI helpers
│   ├── client/
│   │   ├── groq.rs     # OpenAI-compatible chat client (Groq, OpenRouter, LM Studio)
│   │   └── openrouter.rs # OpenRouter models catalog API
│   ├── tools/
│   │   ├── definitions.rs  # Tool schemas
│   │   ├── executor.rs     # Tool dispatcher
│   │   ├── memory.rs       # Memory tool handlers
│   │   ├── cargo.rs        # Structured Cargo and diagnostics
│   │   ├── rust.rs         # rustc explanations and project snapshots
│   │   ├── file_ops.rs     # File operations
│   │   ├── shell.rs        # Shell execution
│   │   ├── grep.rs         # Code search
│   │   ├── search.rs       # Google search
│   │   └── project.rs      # Project structure
│   └── shell/
│       └── persistent.rs   # Persistent shell
```

## Rust Diagnostic Workflow

For Rust projects, Vybrid is expected to inspect crate shape with `rust_project_snapshot` or `cargo_metadata`, run `run_cargo` for `check`, `test`, `clippy`, and `fmt`, and use `diagnostic_format=json` when compiler output needs structured summaries. `explain_rust_diagnostic` can expand rustc codes such as `E0382`, `E0499`, and `E0502`.

When the Rust LSP is connected, Vybrid can also use `rust_lsp_query` for lower-latency editor intelligence:

- **`status`**: Report whether `rust-analyzer` is off, connecting, connected, or in an error state.
- **`diagnostics`**: Return diagnostics published by `rust-analyzer`, optionally scoped to one Rust file.
- **`hover`**: Ask for type, symbol, and documentation information at a file position.
- **`definition`**: Resolve the definition location for a symbol.
- **`references`**: Find symbol references, including declarations.
- **`document_symbols`**: List symbols in a Rust source file.
- **`completion`**: Request completions at a file position.
- **`code_actions`**: Request available code actions at a file position.
- **`formatting`**: Ask `rust-analyzer` for formatting text edits.

LSP results are bounded before being returned to the model, files are opened lazily only when queried, and LSP request timeouts prevent a stuck server from hanging the REPL. The compiler-backed tools still matter: use `run_cargo` for authoritative build/test/clippy verification, and use the LSP as a fast, contextual assistant while editing.

## Verification

```bash
cd vybrid-rust
cargo fmt --check
cargo test
cargo clippy -- -D warnings
```

## Data Directory

- **API keys**: mirrored in **`~/.vybrid/.env`** and **`vybrid-rust/.env`** (same values; use from any cwd)

Vybrid also stores runtime data in `~/.vybrid/`:

- `messages/` — Session transcripts (searchable via memory tools)
- `progress/` — Tool result offloads and request progress
- `cache/` — OpenRouter model catalog cache
- `skills/` — Optional global Agent Skills (also `.vybrid/skills/` per project)

## Changelog

### Unreleased

- **Agent Skills**: Pi-compatible `SKILL.md` discovery, progressive system-prompt disclosure, `/skills` management commands, and `/skill:<name>` loading. Example skill: `vybrid-rust/skills/rust-compile-fix-loop/`.
- **`/compact`**: Manual LLM summarization of older conversation history to free context while preserving recent messages.
- README: `/help` menu screenshot and expanded command, skills, memory, and compaction documentation.

### 1.1.0

- **OpenRouter**: Added as a third LLM provider with live model catalog, cached lists, tiered `/menu` picker (recommended, search, provider, full catalog), and `OPENROUTER_API_KEY` / `OPENROUTER_MODEL` env support.
- **`/menu`**: Reorganized into provider submenus (Groq, OpenRouter, LM Studio, SerpAPI, Rust LSP) with scrollable model selection.
- Rust LSP: added optional `rust-analyzer` integration through `/menu`, prompt status indicator, persisted LSP settings, and the `rust_lsp_query` tool.
- README: documented **`VYBRID_LLM_PROVIDER`** and keeping Groq + LM Studio credentials in the same env files.
- CLI: spinner label is **`groq`**, **`openrouter`**, or **`local`** (LM Studio) depending on the active provider.

## License

MIT License

## Credits

Default inference uses [Groq](https://groq.com/) with [`openai/gpt-oss-120b`](https://console.groq.com/docs/model/openai/gpt-oss-120b) ([OpenAI-compatible API](https://console.groq.com/docs/openai)). You can instead use [OpenRouter](https://openrouter.ai/) for access to hundreds of cloud models, or a local model via [LM Studio](https://lmstudio.ai/) ([OpenAI compatibility](https://lmstudio.ai/docs/developer/openai-compat)).
