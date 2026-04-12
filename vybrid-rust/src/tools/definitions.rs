use crate::client::groq::{FunctionDef, Tool};
use serde_json::json;

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
                            "description": "The path to the file to read (relative or absolute)"
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
                    "anyOf": [
                        {
                            "type": "object",
                            "properties": {
                                "file_path": {
                                    "type": "string",
                                    "description": "Path where the file should be created"
                                },
                                "content": {
                                    "type": "string",
                                    "description": "Content to write"
                                }
                            },
                            "required": ["file_path", "content"]
                        },
                        {
                            "type": "object",
                            "properties": {
                                "path": {
                                    "type": "string",
                                    "description": "Path where the file should be created (alias for file_path)"
                                },
                                "content": {
                                    "type": "string",
                                    "description": "Content to write"
                                }
                            },
                            "required": ["path", "content"]
                        }
                    ]
                }),
            },
        },
        Tool {
            tool_type: "function".to_string(),
            function: FunctionDef {
                name: "create_multiple_files".to_string(),
                description: "Create multiple files at once".to_string(),
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
                                "required": ["path", "content"]
                            },
                            "description": "Array of files to create with their paths and content"
                        }
                    },
                    "required": ["files"]
                }),
            },
        },
        // File editing
        Tool {
            tool_type: "function".to_string(),
            function: FunctionDef {
                name: "edit_file".to_string(),
                description: "Edit an existing file by replacing a specific snippet with new content. Provide the file as either `path` OR `file_path` (both are accepted; use whichever matches your habit).".to_string(),
                parameters: json!({
                    "anyOf": [
                        {
                            "type": "object",
                            "properties": {
                                "path": {
                                    "type": "string",
                                    "description": "Path to the file to edit (relative or absolute)"
                                },
                                "original_snippet": {
                                    "type": "string",
                                    "description": "The exact text snippet to find and replace"
                                },
                                "new_snippet": {
                                    "type": "string",
                                    "description": "The new text to replace the original snippet with"
                                }
                            },
                            "required": ["path", "original_snippet", "new_snippet"]
                        },
                        {
                            "type": "object",
                            "properties": {
                                "file_path": {
                                    "type": "string",
                                    "description": "Path to the file to edit (relative or absolute)"
                                },
                                "original_snippet": {
                                    "type": "string",
                                    "description": "The exact text snippet to find and replace"
                                },
                                "new_snippet": {
                                    "type": "string",
                                    "description": "The new text to replace the original snippet with"
                                }
                            },
                            "required": ["file_path", "original_snippet", "new_snippet"]
                        }
                    ]
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
                        "working_directory": {
                            "type": "string",
                            "description": "Directory to run cargo in (optional; defaults to current directory)"
                        }
                    },
                    "required": ["subcommand"]
                }),
            },
        },
        // Enhanced grep
        Tool {
            tool_type: "function".to_string(),
            function: FunctionDef {
                name: "enhanced_grep".to_string(),
                description: "Search files with a regex pattern. Specify where to search using exactly one of: file_paths (array), file_path (string), or path (string — same meaning as file_path; common model alias).".to_string(),
                parameters: json!({
                    "anyOf": [
                        {
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
                                "context_lines": { "type": "integer", "description": "Context lines before/after matches (default 3)" },
                                "case_sensitive": { "type": "boolean", "description": "Case-sensitive search (default false)" },
                                "max_matches": { "type": "integer", "description": "Max matches per file (default 20)" }
                            },
                            "required": ["pattern", "file_paths"]
                        },
                        {
                            "type": "object",
                            "properties": {
                                "pattern": { "type": "string", "description": "Search pattern (regex)." },
                                "file_path": {
                                    "type": "string",
                                    "description": "Single file or glob to search."
                                },
                                "context_lines": { "type": "integer", "description": "Context lines before/after matches (default 3)" },
                                "case_sensitive": { "type": "boolean", "description": "Case-sensitive search (default false)" },
                                "max_matches": { "type": "integer", "description": "Max matches per file (default 20)" }
                            },
                            "required": ["pattern", "file_path"]
                        },
                        {
                            "type": "object",
                            "properties": {
                                "pattern": { "type": "string", "description": "Search pattern (regex)." },
                                "path": {
                                    "type": "string",
                                    "description": "Single file or glob (alias for file_path; same as edit_file)."
                                },
                                "context_lines": { "type": "integer", "description": "Context lines before/after matches (default 3)" },
                                "case_sensitive": { "type": "boolean", "description": "Case-sensitive search (default false)" },
                                "max_matches": { "type": "integer", "description": "Max matches per file (default 20)" }
                            },
                            "required": ["pattern", "path"]
                        }
                    ]
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
