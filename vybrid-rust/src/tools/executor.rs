#![allow(dead_code)]

use anyhow::{anyhow, Result};
use serde_json::Value;

use crate::lsp::{RustLspManager, RustLspOperation, RustLspQuery};
use crate::memory::MemoryStore;

use super::{cargo, file_ops, grep, memory, output, project, rust, search, shell};

#[derive(Clone, Default)]
pub struct ToolRuntime {
    pub rust_lsp: Option<RustLspManager>,
    pub memory: Option<MemoryStore>,
    pub file_read_cache: file_ops::FileReadCache,
    pub output_store: output::ToolOutputStore,
}

/// Tools that never mutate project or session state. Rounds consisting solely of
/// these can run concurrently without changing observable behavior.
pub fn is_read_only_tool(name: &str) -> bool {
    matches!(
        name,
        "read_file"
            | "read_multiple_files"
            | "enhanced_grep"
            | "cargo_metadata"
            | "rust_project_snapshot"
            | "explain_rust_diagnostic"
            | "rust_lsp_query"
            | "read_project_index"
            | "list_memory_topics"
            | "read_memory_topic"
            | "search_memory_transcripts"
            | "google_search"
            | "get_current_todo_items"
    )
}

/// Execute a tool by name with given arguments
pub async fn execute_tool(name: &str, arguments: &str) -> Result<String> {
    let runtime = ToolRuntime::default();
    execute_tool_with_context(name, arguments, &runtime).await
}

pub async fn execute_tool_with_context(
    name: &str,
    arguments: &str,
    runtime: &ToolRuntime,
) -> Result<String> {
    // Tool arguments arrive as a JSON string from the model. Treat malformed
    // JSON as recoverable feedback instead of silently executing with `{}`.
    let args: Value = serde_json::from_str(arguments).map_err(|e| {
        anyhow!(
            "tool `{}` received invalid JSON arguments: {}. Use a smaller tool call, split large edits into steps, or provide the patch as text.",
            name,
            e
        )
    })?;

    let result = match name {
        // File reading
        "read_file" => {
            let path = args["file_path"].as_str().unwrap_or("");
            let start_line = args["start_line"].as_u64().map(|n| n as usize);
            let line_count = args["line_count"].as_u64().map(|n| n as usize);
            let max_bytes = args["max_bytes"].as_u64().map(|n| n as usize);
            file_ops::read_file_with_options_cached(
                path,
                start_line,
                line_count,
                max_bytes,
                Some(&runtime.file_read_cache),
            )
        }

        "read_multiple_files" => {
            let paths: Vec<&str> = args["file_paths"]
                .as_array()
                .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect())
                .unwrap_or_default();
            file_ops::read_multiple_files_cached(&paths, Some(&runtime.file_read_cache))
        }

        // File creation
        "create_file" => {
            let path = args["path"]
                .as_str()
                .or_else(|| args["file_path"].as_str())
                .unwrap_or("");
            let content = args["content"].as_str().unwrap_or("");
            file_ops::create_file(path, content)
        }

        "create_multiple_files" => {
            let Some(files_array) = args["files"].as_array() else {
                return Err(anyhow!(
                    "create_multiple_files: missing files array — provide [{{\"path\":\"...\",\"content\":\"...\"}}]. For large or multiline content, use smaller create_file calls."
                ));
            };
            if files_array.is_empty() {
                return Err(anyhow!(
                    "create_multiple_files: files array was empty — provide at least one file object"
                ));
            }
            let mut files: Vec<(String, String)> = Vec::new();
            for (idx, f) in files_array.iter().enumerate() {
                let path = f["path"].as_str().unwrap_or("").trim();
                if path.is_empty() {
                    return Err(anyhow!(
                        "create_multiple_files: files[{idx}] is missing path"
                    ));
                }
                let Some(content) = f["content"].as_str() else {
                    return Err(anyhow!(
                        "create_multiple_files: files[{idx}] is missing content"
                    ));
                };
                files.push((path.to_string(), content.to_string()));
            }
            file_ops::create_multiple_files(&files)
        }

        // File editing
        "edit_file" => {
            let path = args["path"]
                .as_str()
                .or_else(|| args["file_path"].as_str())
                .unwrap_or("");
            let original = args["original_snippet"]
                .as_str()
                .or_else(|| args["old_string"].as_str())
                .unwrap_or("");
            let new = args["new_snippet"]
                .as_str()
                .or_else(|| args["new_string"].as_str())
                .unwrap_or("");
            if path.is_empty() {
                return Err(anyhow!(
                    "edit_file: missing path or file_path — provide the file to edit"
                ));
            }
            if original.is_empty() {
                return Err(anyhow!(
                    "edit_file: missing original_snippet or old_string — exact text to find in the file"
                ));
            }
            let dry_run = args["dry_run"].as_bool().unwrap_or(false);
            let context_before = args["context_before"].as_str();
            let context_after = args["context_after"].as_str();
            file_ops::edit_file_with_context_options(
                path,
                original,
                new,
                dry_run,
                context_before,
                context_after,
            )
        }

        // Shell execution
        "execute_bash_command" => {
            let command = args["command"].as_str().unwrap_or("");
            let description = args["description"].as_str();
            let working_dir = args["working_directory"].as_str();
            shell::execute_bash(command, description, working_dir).await
        }

        "run_cargo" => {
            let subcommand = args["subcommand"].as_str().unwrap_or("");
            if subcommand.trim().is_empty() {
                return Err(anyhow!(
                    "run_cargo: missing subcommand — provide check, build, test, clippy, fmt, doc, run, or another Cargo subcommand"
                ));
            }
            let release = args["release"].as_bool().unwrap_or(false);
            let package = args["package"].as_str();
            let manifest_path = args["manifest_path"].as_str();
            let diagnostic_format = cargo::DiagnosticFormat::parse(
                args["diagnostic_format"]
                    .as_str()
                    .or_else(|| args["message_format"].as_str()),
            );
            let extra: Vec<String> = args["extra_args"]
                .as_array()
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            let working_dir = args["working_directory"].as_str();
            cargo::run_cargo(
                subcommand,
                release,
                package,
                manifest_path,
                &extra,
                working_dir,
                diagnostic_format,
            )
            .await
        }

        "explain_rust_diagnostic" => {
            let code_or_topic = args["code_or_topic"].as_str().unwrap_or("");
            rust::explain_rust_diagnostic(code_or_topic).await
        }

        "cargo_metadata" => {
            let manifest_path = args["manifest_path"].as_str();
            let working_dir = args["working_directory"].as_str();
            rust::cargo_metadata(manifest_path, working_dir).await
        }

        "rust_project_snapshot" => {
            let manifest_path = args["manifest_path"].as_str();
            let working_dir = args["working_directory"].as_str();
            rust::rust_project_snapshot(manifest_path, working_dir).await
        }

        "rust_lsp_query" => {
            let operation = args["operation"]
                .as_str()
                .and_then(RustLspOperation::parse)
                .ok_or_else(|| {
                    anyhow!(
                        "rust_lsp_query: operation must be one of status, diagnostics, hover, definition, references, document_symbols, completion, code_actions, formatting"
                    )
                })?;
            let line = args["line"].as_u64().map(|n| n as u32);
            let character = args["character"].as_u64().map(|n| n as u32);
            let file_path = args["file_path"]
                .as_str()
                .or_else(|| args["path"].as_str())
                .map(str::to_string);
            let Some(rust_lsp) = runtime.rust_lsp.as_ref() else {
                return Ok("Rust LSP is not available in this session.".to_string());
            };
            rust_lsp
                .query(RustLspQuery {
                    operation,
                    file_path,
                    line,
                    character,
                })
                .await
        }

        "read_project_index" => crate::project_index::read_project_index(),

        "generate_project_index" => {
            let force = args["force"].as_bool().unwrap_or(false);
            crate::project_index::generate_project_index(force)
        }

        "list_memory_topics" => memory::list_memory_topics(runtime.memory.as_ref()),

        "read_memory_topic" => {
            let topic = args["topic"].as_str().unwrap_or("");
            memory::read_memory_topic(runtime.memory.as_ref(), topic)
        }

        "search_memory_transcripts" => {
            let query = args["query"].as_str().unwrap_or("");
            let case_sensitive = args["case_sensitive"].as_bool().unwrap_or(false);
            let max_matches = args["max_matches"].as_u64().unwrap_or(10).min(20) as usize;
            memory::search_memory_transcripts(
                runtime.memory.as_ref(),
                query,
                case_sensitive,
                max_matches,
            )
        }

        // Enhanced grep
        "enhanced_grep" => {
            let pattern = args["pattern"].as_str().unwrap_or("");
            let mut paths: Vec<&str> = args["file_paths"]
                .as_array()
                .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect())
                .unwrap_or_default();
            if paths.is_empty() {
                if let Some(p) = args["file_path"].as_str() {
                    if !p.is_empty() {
                        paths.push(p);
                    }
                }
            }
            if paths.is_empty() {
                if let Some(p) = args["path"].as_str() {
                    if !p.is_empty() {
                        paths.push(p);
                    }
                }
            }
            if pattern.is_empty() {
                return Err(anyhow!("enhanced_grep: missing pattern"));
            }
            if paths.is_empty() {
                return Err(anyhow!(
                    "enhanced_grep: provide file_paths (array), file_path, or path (string)"
                ));
            }
            let context = args["context_lines"].as_u64().unwrap_or(1).min(5) as usize;
            let case_sensitive = args["case_sensitive"].as_bool().unwrap_or(false);
            let max_matches = args["max_matches"].as_u64().unwrap_or(10).min(50) as usize;
            grep::enhanced_grep(pattern, &paths, context, case_sensitive, max_matches)
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
    };

    result.and_then(|output| runtime.output_store.maybe_offload(name, output))
}

/// Execute a tool synchronously
pub fn execute_tool_sync(name: &str, arguments: &str) -> Result<String> {
    // Create a runtime for async tools
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;

    rt.block_on(execute_tool(name, arguments))
}
