use crate::client::groq::{FunctionDef, Tool};
use serde_json::json;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum ToolProfile {
    Full,
    Navigate,
    Edit,
    Build,
    None,
}

pub fn get_tools_for_profile(profile: ToolProfile) -> Vec<Tool> {
    let names: &[&str] = match profile {
        ToolProfile::Full => return get_all_tools(),
        ToolProfile::Navigate => &[
            "read_project_index",
            "generate_project_index",
            "read_file",
            "read_multiple_files",
            "enhanced_grep",
            "rust_project_snapshot",
            "cargo_metadata",
            "run_cargo",
            "rust_lsp_query",
        ],
        ToolProfile::Edit => &[
            "read_project_index",
            "read_file",
            "read_multiple_files",
            "enhanced_grep",
            "edit_file",
            "create_file",
            "run_cargo",
            "cargo_metadata",
            "rust_project_snapshot",
            "rust_lsp_query",
        ],
        ToolProfile::Build => &[
            "run_cargo",
            "cargo_metadata",
            "rust_project_snapshot",
            "rust_lsp_query",
            "read_file",
        ],
        ToolProfile::None => &[],
    };

    get_all_tools()
        .into_iter()
        .filter(|tool| names.contains(&tool.function.name.as_str()))
        .collect()
}

/// Get all available tools for function calling
pub fn get_all_tools() -> Vec<Tool> {
    vec![
        // File reading
        Tool {
            tool_type: "function".to_string(),
            function: FunctionDef {
                name: "read_file".to_string(),
                description: "Read the content of a single file from the filesystem".to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "file_path": {
                            "type": "string",
                            "description": "The path to the file to read (root-relative or absolute)"
                        },
                        "start_line": {
                            "type": "integer",
                            "description": "Optional 1-based first line to return"
                        },
                        "line_count": {
                            "type": "integer",
                            "description": "Optional number of lines to return"
                        },
                        "max_bytes": {
                            "type": "integer",
                            "description": "Optional output byte cap for this read"
                        }
                    },
                    "required": ["file_path"]
                }),
            },
        },
        Tool {
            tool_type: "function".to_string(),
            function: FunctionDef {
                name: "read_multiple_files".to_string(),
                description: "Read the content of multiple files from the filesystem".to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "file_paths": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "Array of file paths to read (relative or absolute)"
                        }
                    },
                    "required": ["file_paths"]
                }),
            },
        },
        // File creation
        Tool {
            tool_type: "function".to_string(),
            function: FunctionDef {
                name: "create_file".to_string(),
                description: "Create a new file or overwrite an existing file with the provided content. Use `file_path` or `path` for the destination (same meaning).".to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "file_path": {
                            "type": "string",
                            "description": "Path where the file should be created"
                        },
                        "path": {
                            "type": "string",
                            "description": "Path where the file should be created (alias for file_path)"
                        },
                        "content": {
                            "type": "string",
                            "description": "Content to write"
                        }
                    },
                    "description": "Provide `content` and either `file_path` or `path`. (Loose keys so mixed model outputs pass API validation; missing fields are rejected when the tool runs.)"
                }),
            },
        },
        Tool {
            tool_type: "function".to_string(),
            function: FunctionDef {
                name: "create_multiple_files".to_string(),
                description: "Create multiple files at once. Provide `files` as an array of objects with `path` and `content`. For large or multiline content, prefer separate smaller `create_file` calls.".to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "files": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "path": { "type": "string" },
                                    "content": { "type": "string" }
                                },
                                "description": "One file to create. Provide both `path` and `content`; missing fields are rejected when the tool runs."
                            },
                            "description": "Array of files to create with their paths and content"
                        }
                    },
                    "description": "Provide `files` with `path` and `content` for each item. The schema is intentionally loose so provider validation does not fail before Vybrid can return a recoverable tool error."
                }),
            },
        },
        // File editing
        Tool {
            tool_type: "function".to_string(),
            function: FunctionDef {
                name: "edit_file".to_string(),
                description: "Replace an exact snippet in a file. File: `path` or `file_path`. Snippet to find: `original_snippet` or `old_string`. Replacement: `new_snippet` or `new_string`. If the snippet appears more than once, add `context_before` and/or `context_after` with nearby exact text to identify the intended occurrence. You may mix naming styles (e.g. `path` + `old_string` + `new_string`).".to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "File to edit" },
                        "file_path": { "type": "string", "description": "File to edit (alias of path)" },
                        "original_snippet": { "type": "string", "description": "Exact text to find" },
                        "new_snippet": { "type": "string", "description": "Replacement text" },
                        "old_string": { "type": "string", "description": "Exact text to find (alias of original_snippet)" },
                        "new_string": { "type": "string", "description": "Replacement text (alias of new_snippet)" },
                        "context_before": { "type": "string", "description": "Optional exact text near and before the intended occurrence, used only to disambiguate repeated snippets." },
                        "context_after": { "type": "string", "description": "Optional exact text near and after the intended occurrence, used only to disambiguate repeated snippets." },
                        "dry_run": { "type": "boolean", "description": "If true, validate and preview the edit without writing the file." }
                    },
                    "description": "Provide a file (`path` or `file_path`) and the before/after text using either naming style. Use `context_before`/`context_after` when the exact snippet is duplicated. (Single object so API validation accepts alias mixes; missing or invalid combos fail with a clear error when the tool runs.)"
                }),
            },
        },
        // Shell execution
        Tool {
            tool_type: "function".to_string(),
            function: FunctionDef {
                name: "execute_bash_command".to_string(),
                description: "Execute bash/shell commands in the terminal. Use this to run system commands, manage processes, install packages, run build scripts, or perform any terminal operations.".to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "command": {
                            "type": "string",
                            "description": "The bash/shell command to execute. Can be a single command or multiple commands separated by && or ;"
                        },
                        "description": {
                            "type": "string",
                            "description": "Human-readable description of what this command accomplishes (optional)"
                        },
                        "working_directory": {
                            "type": "string",
                            "description": "Optional working directory to execute the command in. Defaults to current directory."
                        }
                    },
                    "required": ["command"]
                }),
            },
        },
        // Cargo (structured; preferred over raw shell for Rust projects)
        Tool {
            tool_type: "function".to_string(),
            function: FunctionDef {
                name: "run_cargo".to_string(),
                description: "Run Cargo with structured arguments (no shell). Preferred over execute_bash_command for cargo check, build, test, clippy, fmt, doc, etc. Args are passed as argv; do not use shell metacharacters in extra_args.".to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "subcommand": {
                            "type": "string",
                            "description": "Cargo subcommand: check, build, test, run, clippy, fmt, doc, clean, metadata, etc."
                        },
                        "release": {
                            "type": "boolean",
                            "description": "Pass --release when applicable (build, test, run, check, …). Default false."
                        },
                        "package": {
                            "type": "string",
                            "description": "Workspace package name for -p (optional)"
                        },
                        "manifest_path": {
                            "type": "string",
                            "description": "Path to Cargo.toml for --manifest-path (optional)"
                        },
                        "extra_args": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "Additional argv tokens after fixed flags (e.g. test filter, \"--\", \"--nocapture\"). Literal strings only—no shell."
                        },
                        "diagnostic_format": {
                            "type": "string",
                            "enum": ["human", "json"],
                            "description": "Use \"json\" for compiler-aware rustc/clippy summaries on check/build/test/clippy/doc; default \"human\" preserves normal Cargo output."
                        },
                        "working_directory": {
                            "type": "string",
                            "description": "Directory to run cargo in (optional; defaults to current directory)"
                        }
                    },
                    "description": "Provide `subcommand`; missing values are rejected when the tool runs so the model can recover with a simpler call."
                }),
            },
        },
        Tool {
            tool_type: "function".to_string(),
            function: FunctionDef {
                name: "explain_rust_diagnostic".to_string(),
                description: "Explain a Rust compiler error code (for example E0382, E0499, E0502) or a Rust topic such as traits, enums, lifetimes, or async Send. Uses built-in guidance and `rustc --explain` when available.".to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "code_or_topic": {
                            "type": "string",
                            "description": "Rust error code or topic to explain."
                        }
                    },
                    "required": ["code_or_topic"]
                }),
            },
        },
        Tool {
            tool_type: "function".to_string(),
            function: FunctionDef {
                name: "cargo_metadata".to_string(),
                description: "Return `cargo metadata --format-version=1 --no-deps` JSON for workspace/package discovery before editing Rust projects.".to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "manifest_path": {
                            "type": ["string", "null"],
                            "description": "Optional path to Cargo.toml."
                        },
                        "working_directory": {
                            "type": ["string", "null"],
                            "description": "Optional directory to run cargo metadata in."
                        }
                    }
                }),
            },
        },
        Tool {
            tool_type: "function".to_string(),
            function: FunctionDef {
                name: "rust_project_snapshot".to_string(),
                description: "Summarize Rust workspace/package shape: edition, targets, features, dependencies, and workspace member count.".to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "manifest_path": {
                            "type": ["string", "null"],
                            "description": "Optional path to Cargo.toml."
                        },
                        "working_directory": {
                            "type": ["string", "null"],
                            "description": "Optional directory to run cargo metadata in."
                        }
                    }
                }),
            },
        },
        Tool {
            tool_type: "function".to_string(),
            function: FunctionDef {
                name: "rust_lsp_query".to_string(),
                description: "Query the optional connected Rust LSP (rust-analyzer). Always include `operation`: one of status, diagnostics, hover, definition, references, document_symbols, completion, code_actions, formatting. Use 0-based line and character positions for position-based operations.".to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "operation": {
                            "type": "string",
                            "enum": [
                                "status",
                                "diagnostics",
                                "hover",
                                "definition",
                                "references",
                                "document_symbols",
                                "completion",
                                "code_actions",
                                "formatting"
                            ],
                            "description": "The Rust LSP operation to run."
                        },
                        "file_path": {
                            "type": ["string", "null"],
                            "description": "Rust source file path. Required for all operations except status; optional for diagnostics."
                        },
                        "path": {
                            "type": ["string", "null"],
                            "description": "Alias for file_path."
                        },
                        "line": {
                            "type": ["integer", "null"],
                            "description": "0-based line number. Required for hover, definition, references, completion, and code_actions."
                        },
                        "character": {
                            "type": ["integer", "null"],
                            "description": "0-based UTF-16-ish character offset for the LSP position. Required for hover, definition, references, completion, and code_actions."
                        }
                    },
                    "description": "Provide `operation` for every call. The schema is intentionally loose so provider validation does not fail before Vybrid can return a recoverable tool error."
                }),
            },
        },
        Tool {
            tool_type: "function".to_string(),
            function: FunctionDef {
                name: "read_project_index".to_string(),
                description: "Read the compact root-level index.md for cheap project navigation before broad searches or full-file reads.".to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": {},
                    "required": []
                }),
            },
        },
        Tool {
            tool_type: "function".to_string(),
            function: FunctionDef {
                name: "generate_project_index".to_string(),
                description: "Generate or refresh a compact root-level index.md with root-relative paths. Use before broad navigation when index.md is missing or stale.".to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "force": {
                            "type": "boolean",
                            "description": "Overwrite an existing Vybrid-generated index.md. Default false; hand-written files are protected."
                        }
                    },
                    "required": []
                }),
            },
        },
        // Enhanced grep
        Tool {
            tool_type: "function".to_string(),
            function: FunctionDef {
                name: "enhanced_grep".to_string(),
                description: "Search files with a regex pattern. Provide `pattern` and at least one of `file_paths` (array), `file_path`, or `path` (string; same meaning as file_path). The API must accept mixed shapes — use any of these keys; the client validates before running the search.".to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "pattern": {
                            "type": "string",
                            "description": "Search pattern (regex)."
                        },
                        "file_paths": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "Paths or globs to search (one or more)."
                        },
                        "file_path": {
                            "type": "string",
                            "description": "Single file or glob to search."
                        },
                        "path": {
                            "type": "string",
                            "description": "Single file or glob (alias for file_path)."
                        },
                        "context_lines": { "type": "integer", "description": "Context lines before/after matches (default 3)" },
                        "case_sensitive": { "type": "boolean", "description": "Case-sensitive search (default false)" },
                        "max_matches": { "type": "integer", "description": "Max matches per file (default 20)" }
                    }
                }),
            },
        },
        // Google search
        Tool {
            tool_type: "function".to_string(),
            function: FunctionDef {
                name: "google_search".to_string(),
                description: "Search Google using SerpAPI to find information that can help with decision making and completing user intentions.".to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "query": {
                            "type": "string",
                            "description": "The search query to send to Google. Use descriptive keywords for better results."
                        },
                        "num_results": {
                            "type": "integer",
                            "description": "Number of search results to return (default: 10, max: 20)"
                        }
                    },
                    "required": ["query"]
                }),
            },
        },
        // Project structure
        Tool {
            tool_type: "function".to_string(),
            function: FunctionDef {
                name: "create_project_structure".to_string(),
                description: "Create or update the mandatory project structure files (tasks/todo.md, docs/activity.md, and docs/PROJECT_README.md). Use this at the start of any project work.".to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "project_name": {
                            "type": "string",
                            "description": "Optional name of the project for customizing the templates"
                        },
                        "overwrite_existing": {
                            "type": "boolean",
                            "description": "Whether to overwrite existing files (true) or append to them (false). Default is false."
                        }
                    },
                    "required": []
                }),
            },
        },
        Tool {
            tool_type: "function".to_string(),
            function: FunctionDef {
                name: "get_current_todo_items".to_string(),
                description: "Read the current todo.md file and return a list of incomplete tasks.".to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": {},
                    "required": []
                }),
            },
        },
        Tool {
            tool_type: "function".to_string(),
            function: FunctionDef {
                name: "mark_todo_complete".to_string(),
                description: "Mark a specific todo item as complete by updating the checkbox from [ ] to [x] in todo.md.".to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "task_description": {
                            "type": "string",
                            "description": "The description of the task to mark as complete (should match the text in todo.md)"
                        }
                    },
                    "required": ["task_description"]
                }),
            },
        },
    ]
}
