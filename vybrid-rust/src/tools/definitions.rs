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
                description: "Create a new file or overwrite an existing file with the provided content".to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "file_path": {
                            "type": "string",
                            "description": "The path where the file should be created"
                        },
                        "content": {
                            "type": "string",
                            "description": "The content to write to the file"
                        }
                    },
                    "required": ["file_path", "content"]
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
                description: "Edit an existing file by replacing a specific snippet with new content. Use path or file_path for the file (same meaning; path matches create_multiple_files).".to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "Path to the file to edit (relative or absolute)"
                        },
                        "file_path": {
                            "type": "string",
                            "description": "Same as path — include path or file_path (either is valid)"
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
                    "required": ["original_snippet", "new_snippet"]
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
        // Enhanced grep
        Tool {
            tool_type: "function".to_string(),
            function: FunctionDef {
                name: "enhanced_grep".to_string(),
                description: "Advanced grep/search functionality with context lines and enhanced readability for code analysis. Searches for patterns in files with intelligent formatting.".to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "pattern": {
                            "type": "string",
                            "description": "The search pattern (supports regex). Use simple strings for literal matches or regex patterns for advanced searches."
                        },
                        "file_paths": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "List of file paths to search in. Can include individual files or use wildcards (*.py, *.js, etc.)"
                        },
                        "context_lines": {
                            "type": "integer",
                            "description": "Number of context lines to show before and after each match (default: 3)"
                        },
                        "case_sensitive": {
                            "type": "boolean",
                            "description": "Whether the search should be case-sensitive (default: false)"
                        },
                        "max_matches": {
                            "type": "integer",
                            "description": "Maximum number of matches to return per file (default: 20)"
                        }
                    },
                    "required": ["pattern", "file_paths"]
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
