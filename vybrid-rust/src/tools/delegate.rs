//! Daemon delegation module for automatic task distribution
//!
//! This module provides functionality to delegate tasks to background daemon workers
//! automatically through the tool-calling interface.

use anyhow::Result;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::config::Config;
use crate::daemon::queue::{ExecutionRequest, ExecutionResponse, MessageQueue};

/// Configuration for a delegation request
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DelegationConfig {
    /// The task description to delegate
    pub task: String,
    /// Priority level (1 = highest, 5 = lowest)
    pub priority: i32,
    /// Whether to wait for the result or fire-and-forget
    pub wait_for_result: bool,
    /// Timeout in seconds (only applies if wait_for_result is true)
    pub timeout_secs: u64,
}

impl Default for DelegationConfig {
    fn default() -> Self {
        Self {
            task: String::new(),
            priority: 1,
            wait_for_result: true,
            timeout_secs: 300, // 5 minutes
        }
    }
}

/// Result of a delegation operation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DelegationResult {
    pub request_id: String,
    pub status: DelegationStatus,
    pub result: Option<String>,
    pub processing_time: Option<f64>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum DelegationStatus {
    Queued,
    Processing,
    Completed,
    Failed,
    Cancelled,
    Timeout,
}

/// Delegate a task to the daemon pool
///
/// This function sends a task to the background daemon workers and optionally
/// waits for the result. It performs all necessary checks before delegation.
pub async fn delegate_task(config: &Config, delegation_config: DelegationConfig) -> Result<DelegationResult> {
    // Double-check daemon availability (defense in depth)
    let availability = config.check_daemon_availability();
    if !availability.available {
        return Ok(DelegationResult {
            request_id: String::new(),
            status: DelegationStatus::Failed,
            result: None,
            processing_time: None,
            error: Some(format!(
                "Daemon is not available: {}",
                availability.reason.unwrap_or_else(|| "Unknown reason".to_string())
            )),
        });
    }

    // Get current working directory
    let cwd = std::env::current_dir()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| ".".to_string());

    // Create execution request
    let request = ExecutionRequest {
        id: Uuid::new_v4().to_string(),
        timestamp: Utc::now().to_rfc3339(),
        user_query: delegation_config.task.clone(),
        suggested_actions: Vec::new(),
        current_directory: cwd,
        chat_session_id: Uuid::new_v4().to_string(),
        priority: delegation_config.priority,
    };

    let request_id = request.id.clone();

    // Create message queue
    let queue = MessageQueue::new(config.messages_dir.clone(), config.progress_dir.clone());

    // Send request to queue
    queue.send_request(&request)?;

    // If fire-and-forget, return immediately
    if !delegation_config.wait_for_result {
        return Ok(DelegationResult {
            request_id,
            status: DelegationStatus::Queued,
            result: Some("Task queued for background processing.".to_string()),
            processing_time: None,
            error: None,
        });
    }

    // Wait for response with progress tracking
    wait_for_response(config, &request_id, delegation_config.timeout_secs).await
}

/// Wait for a daemon response with progress tracking
async fn wait_for_response(
    config: &Config,
    request_id: &str,
    timeout_secs: u64,
) -> Result<DelegationResult> {
    let start = std::time::Instant::now();
    let poll_interval = std::time::Duration::from_millis(500);

    let response_path = config
        .messages_dir
        .join(format!("response_{}.json", request_id));
    let progress_path = config
        .progress_dir
        .join(format!("progress_{}.json", request_id));
    let request_path = config
        .messages_dir
        .join(format!("request_{}.json", request_id));

    loop {
        // Check timeout
        if start.elapsed().as_secs() >= timeout_secs {
            // Clean up request file on timeout
            let _ = std::fs::remove_file(&request_path);
            let _ = std::fs::remove_file(&progress_path);

            return Ok(DelegationResult {
                request_id: request_id.to_string(),
                status: DelegationStatus::Timeout,
                result: None,
                processing_time: Some(start.elapsed().as_secs_f64()),
                error: Some(format!("Delegation timeout after {} seconds", timeout_secs)),
            });
        }

        // Check for response
        if response_path.exists() {
            match std::fs::read_to_string(&response_path) {
                Ok(content) => {
                    if let Ok(response) = serde_json::from_str::<ExecutionResponse>(&content) {
                        // Clean up files
                        let _ = std::fs::remove_file(&request_path);
                        let _ = std::fs::remove_file(&response_path);
                        let _ = std::fs::remove_file(&progress_path);

                        let status = match response.status.as_str() {
                            "success" => DelegationStatus::Completed,
                            "error" => DelegationStatus::Failed,
                            "cancelled" => DelegationStatus::Cancelled,
                            _ => DelegationStatus::Failed,
                        };

                        let error = if status == DelegationStatus::Failed {
                            Some(response.result.clone())
                        } else {
                            None
                        };

                        let result = if status == DelegationStatus::Completed {
                            Some(response.result)
                        } else {
                            None
                        };

                        return Ok(DelegationResult {
                            request_id: request_id.to_string(),
                            status,
                            result,
                            processing_time: response.processing_time,
                            error,
                        });
                    }
                }
                Err(_) => {
                    // File might be in the middle of being written, continue polling
                }
            }
        }

        tokio::time::sleep(poll_interval).await;
    }
}

/// Format delegation result for display to the AI/user
pub fn format_delegation_result(result: &DelegationResult) -> String {
    let mut output = String::new();

    output.push_str(&format!("Delegation ID: {}\n", &result.request_id[..8.min(result.request_id.len())]));
    output.push_str(&format!("Status: {:?}\n", result.status));

    if let Some(time) = result.processing_time {
        output.push_str(&format!("Processing Time: {:.2}s\n", time));
    }

    match result.status {
        DelegationStatus::Completed => {
            if let Some(ref res) = result.result {
                output.push_str("\n--- Daemon Response ---\n");
                output.push_str(res);
                output.push_str("\n--- End Response ---\n");
            }
        }
        DelegationStatus::Queued => {
            output.push_str("Task has been queued for background processing.\n");
            output.push_str("Use /daemon-status to check progress.\n");
        }
        DelegationStatus::Failed => {
            if let Some(ref err) = result.error {
                output.push_str(&format!("\nError: {}\n", err));
            }
        }
        DelegationStatus::Timeout => {
            output.push_str("\nThe daemon did not respond within the timeout period.\n");
            output.push_str("The task may still be processing in the background.\n");
        }
        DelegationStatus::Cancelled => {
            output.push_str("\nThe task was cancelled.\n");
        }
        DelegationStatus::Processing => {
            output.push_str("\nTask is currently being processed by daemon workers.\n");
        }
    }

    output
}

/// Check the status of a previously delegated task
#[allow(dead_code)]
pub fn check_delegation_status(config: &Config, request_id: &str) -> DelegationResult {
    let response_path = config
        .messages_dir
        .join(format!("response_{}.json", request_id));
    let progress_path = config
        .progress_dir
        .join(format!("progress_{}.json", request_id));
    let request_path = config
        .messages_dir
        .join(format!("request_{}.json", request_id));

    // Check if response exists
    if response_path.exists() {
        if let Ok(content) = std::fs::read_to_string(&response_path) {
            if let Ok(response) = serde_json::from_str::<ExecutionResponse>(&content) {
                let status = match response.status.as_str() {
                    "success" => DelegationStatus::Completed,
                    "error" => DelegationStatus::Failed,
                    "cancelled" => DelegationStatus::Cancelled,
                    _ => DelegationStatus::Failed,
                };

                return DelegationResult {
                    request_id: request_id.to_string(),
                    status,
                    result: Some(response.result),
                    processing_time: response.processing_time,
                    error: None,
                };
            }
        }
    }

    // Check if still processing
    if request_path.exists() {
        if progress_path.exists() {
            return DelegationResult {
                request_id: request_id.to_string(),
                status: DelegationStatus::Processing,
                result: None,
                processing_time: None,
                error: None,
            };
        }
        return DelegationResult {
            request_id: request_id.to_string(),
            status: DelegationStatus::Queued,
            result: None,
            processing_time: None,
            error: None,
        };
    }

    // Request not found
    DelegationResult {
        request_id: request_id.to_string(),
        status: DelegationStatus::Failed,
        result: None,
        processing_time: None,
        error: Some("Delegation request not found. It may have already completed and been cleaned up.".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_delegation_config_default() {
        let config = DelegationConfig::default();
        assert_eq!(config.priority, 1);
        assert!(config.wait_for_result);
        assert_eq!(config.timeout_secs, 300);
    }

    #[test]
    fn test_format_delegation_result() {
        let result = DelegationResult {
            request_id: "test-1234-5678".to_string(),
            status: DelegationStatus::Completed,
            result: Some("Task completed successfully".to_string()),
            processing_time: Some(2.5),
            error: None,
        };

        let formatted = format_delegation_result(&result);
        assert!(formatted.contains("test-123"));
        assert!(formatted.contains("Completed"));
        assert!(formatted.contains("2.50s"));
    }
}
