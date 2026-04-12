#![allow(dead_code)]

use anyhow::{anyhow, Result};
use serde_json::Value;

use super::{file_ops, grep, project, search, shell};

/// Execute a tool by name with given arguments
pub async fn execute_tool(name: &str, arguments: &str) -> Result<String> {
    // Parse arguments JSON
    let args: Value = serde_json::from_str(arguments).unwrap_or(Value::Object(serde_json::Map::new()));

    match name {
        // File reading
        "read_file" => {
            let path = args["file_path"].as_str().unwrap_or("");
            file_ops::read_file(path)
        }

        "read_multiple_files" => {
            let paths: Vec<&str> = args["file_paths"]
                .as_array()
                .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect())
                .unwrap_or_default();
            file_ops::read_multiple_files(&paths)
        }

        // File creation
        "create_file" => {
            let path = args["file_path"].as_str().unwrap_or("");
            let content = args["content"].as_str().unwrap_or("");
            file_ops::create_file(path, content)
        }

        "create_multiple_files" => {
            let files: Vec<(String, String)> = args["files"]
                .as_array()
                .map(|arr| {
                    arr.iter()
                        .filter_map(|f| {
                            let path = f["path"].as_str()?.to_string();
                            let content = f["content"].as_str()?.to_string();
                            Some((path, content))
                        })
                        .collect()
                })
                .unwrap_or_default();
            file_ops::create_multiple_files(&files)
        }

        // File editing
        "edit_file" => {
            let path = args["path"]
                .as_str()
                .or_else(|| args["file_path"].as_str())
                .unwrap_or("");
            let original = args["original_snippet"].as_str().unwrap_or("");
            let new = args["new_snippet"].as_str().unwrap_or("");
            if path.is_empty() {
                return Err(anyhow!(
                    "edit_file: missing path or file_path — provide the file to edit"
                ));
            }
            file_ops::edit_file(path, original, new)
        }

        // Shell execution
        "execute_bash_command" => {
            let command = args["command"].as_str().unwrap_or("");
            let description = args["description"].as_str();
            let working_dir = args["working_directory"].as_str();
            shell::execute_bash(command, description, working_dir).await
        }

        // Enhanced grep
        "enhanced_grep" => {
            let pattern = args["pattern"].as_str().unwrap_or("");
            let paths: Vec<&str> = args["file_paths"]
                .as_array()
                .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect())
                .unwrap_or_default();
            let context = args["context_lines"].as_u64().unwrap_or(3) as usize;
            let case_sensitive = args["case_sensitive"].as_bool().unwrap_or(false);
            grep::enhanced_grep(pattern, &paths, context, case_sensitive)
        }

        // Google search
        "google_search" => {
            let query = args["query"].as_str().unwrap_or("");
            let num = args["num_results"].as_u64().unwrap_or(10) as usize;
            search::google_search(query, num).await
        }

        // Project structure
        "create_project_structure" => {
            let name = args["project_name"].as_str();
            let overwrite = args["overwrite_existing"].as_bool().unwrap_or(false);
            project::create_structure(name, overwrite)
        }

        "get_current_todo_items" => project::get_current_todo_items(),

        "mark_todo_complete" => {
            let task = args["task_description"].as_str().unwrap_or("");
            project::mark_todo_complete(task)
        }

        // Unknown tool
        _ => Ok(format!("Unknown tool: {}", name)),
    }
}

/// Execute a tool synchronously
pub fn execute_tool_sync(name: &str, arguments: &str) -> Result<String> {
    // Create a runtime for async tools
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;

    rt.block_on(execute_tool(name, arguments))
}
