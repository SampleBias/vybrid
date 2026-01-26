#![allow(dead_code)]

use anyhow::Result;
use serde_json::Value;

use crate::config::Config;
use super::{delegate, file_ops, grep, project, search, shell};

/// Execute a tool by name with given arguments
/// 
/// # Arguments
/// * `name` - The name of the tool to execute
/// * `arguments` - JSON string of arguments for the tool
/// * `config` - Optional configuration (required for daemon delegation tools)
pub async fn execute_tool(name: &str, arguments: &str, config: Option<&Config>) -> Result<String> {
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
            let path = args["file_path"].as_str().unwrap_or("");
            let original = args["original_snippet"].as_str().unwrap_or("");
            let new = args["new_snippet"].as_str().unwrap_or("");
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

        // Daemon delegation tools
        "delegate_to_daemon" => {
            let cfg = config.ok_or_else(|| anyhow::anyhow!(
                "Configuration required for daemon delegation. Daemon may not be available."
            ))?;

            // Defense in depth: verify daemon is still available
            if !cfg.is_daemon_available() {
                return Ok("Error: Daemon is not currently running. Cannot delegate tasks. \
                          Please start the daemon first with: vybrid → Daemon Mode".to_string());
            }

            let task = args["task"].as_str().unwrap_or("");
            if task.is_empty() {
                return Ok("Error: Task description is required for delegation.".to_string());
            }

            let priority = args["priority"].as_i64().unwrap_or(1) as i32;
            let wait_for_result = args["wait_for_result"].as_bool().unwrap_or(true);
            let timeout_secs = args["timeout_secs"].as_u64().unwrap_or(300);

            let delegation_config = delegate::DelegationConfig {
                task: task.to_string(),
                priority: priority.clamp(1, 5),
                wait_for_result,
                timeout_secs: timeout_secs.clamp(10, 600),
            };

            let result = delegate::delegate_task(cfg, delegation_config).await?;
            Ok(delegate::format_delegation_result(&result))
        }

        "check_daemon_status" => {
            let cfg = config.ok_or_else(|| anyhow::anyhow!(
                "Configuration required for daemon status check."
            ))?;

            let availability = cfg.check_daemon_availability();
            
            let mut output = String::new();
            output.push_str("=== Daemon Pool Status ===\n\n");
            
            if availability.available {
                output.push_str("Status: ● RUNNING\n");
                if let Some(ref status) = availability.status {
                    output.push_str(&format!("PID: {}\n", status.pid));
                    output.push_str(&format!("Workers: {}\n", status.workers));
                    output.push_str(&format!("Session: {}\n", &status.session_id[..8.min(status.session_id.len())]));
                    output.push_str(&format!("Started: {}\n", status.timestamp));
                }
                output.push_str("\nDelegation tools are available.\n");
            } else {
                output.push_str("Status: ○ NOT RUNNING\n");
                if let Some(reason) = availability.reason {
                    output.push_str(&format!("Reason: {}\n", reason));
                }
                output.push_str("\nTo start the daemon: vybrid → Daemon Mode\n");
                output.push_str("Delegation tools are not available until daemon is started.\n");
            }
            
            Ok(output)
        }

        // Unknown tool
        _ => Ok(format!("Unknown tool: {}", name)),
    }
}

/// Execute a tool synchronously (for daemon mode)
/// Note: Daemon mode doesn't support delegation to avoid circular dependencies
pub fn execute_tool_sync(name: &str, arguments: &str) -> Result<String> {
    // Create a runtime for async tools
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;

    // Daemon mode doesn't support delegation (no config passed)
    rt.block_on(execute_tool(name, arguments, None))
}
