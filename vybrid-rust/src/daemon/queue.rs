#![allow(dead_code)]

use anyhow::Result;
use chrono::Utc;
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::PathBuf;
use uuid::Uuid;

/// Execution request from chat mode
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionRequest {
    pub id: String,
    pub timestamp: String,
    pub user_query: String,
    pub suggested_actions: Vec<String>,
    pub current_directory: String,
    pub chat_session_id: String,
    pub priority: i32,
}

impl ExecutionRequest {
    pub fn new(user_query: String, current_directory: String) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            timestamp: Utc::now().to_rfc3339(),
            user_query,
            suggested_actions: Vec::new(),
            current_directory,
            chat_session_id: Uuid::new_v4().to_string(),
            priority: 1,
        }
    }
}

/// Execution response from daemon
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionResponse {
    pub request_id: String,
    pub timestamp: String,
    pub status: String,
    pub result: String,
    pub agent_session_id: String,
    pub processing_time: Option<f64>,
}

impl ExecutionResponse {
    pub fn success(request_id: &str, result: String, session_id: &str, processing_time: f64) -> Self {
        Self {
            request_id: request_id.to_string(),
            timestamp: Utc::now().to_rfc3339(),
            status: "success".to_string(),
            result,
            agent_session_id: session_id.to_string(),
            processing_time: Some(processing_time),
        }
    }

    pub fn error(request_id: &str, error: String, session_id: &str) -> Self {
        Self {
            request_id: request_id.to_string(),
            timestamp: Utc::now().to_rfc3339(),
            status: "error".to_string(),
            result: error,
            agent_session_id: session_id.to_string(),
            processing_time: None,
        }
    }

    pub fn cancelled(request_id: &str, session_id: &str) -> Self {
        Self {
            request_id: request_id.to_string(),
            timestamp: Utc::now().to_rfc3339(),
            status: "cancelled".to_string(),
            result: "Request was cancelled".to_string(),
            agent_session_id: session_id.to_string(),
            processing_time: None,
        }
    }
}

/// Progress update for long-running requests
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProgressUpdate {
    pub request_id: String,
    pub timestamp: String,
    pub stage: String,
    pub progress: Option<f32>,
    pub message: Option<String>,
}

/// Atomic message queue for inter-mode communication
pub struct MessageQueue {
    messages_dir: PathBuf,
    progress_dir: PathBuf,
}

impl MessageQueue {
    pub fn new(messages_dir: PathBuf, progress_dir: PathBuf) -> Self {
        Self {
            messages_dir,
            progress_dir,
        }
    }

    /// Write a request to the queue
    pub fn send_request(&self, request: &ExecutionRequest) -> Result<()> {
        let file_path = self.messages_dir.join(format!("request_{}.json", request.id));
        self.atomic_write(&file_path, request)?;
        Ok(())
    }

    /// Write a response to the queue
    pub fn send_response(&self, response: &ExecutionResponse) -> Result<()> {
        let file_path = self.messages_dir.join(format!("response_{}.json", response.request_id));
        self.atomic_write(&file_path, response)?;
        Ok(())
    }

    /// Get pending requests (requests without responses)
    pub fn get_pending_requests(&self) -> Result<Vec<ExecutionRequest>> {
        let mut requests = Vec::new();
        let timeout_secs = 300; // 5 minute timeout

        for entry in fs::read_dir(&self.messages_dir)? {
            let entry = entry?;
            let path = entry.path();
            
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                if name.starts_with("request_") && name.ends_with(".json") {
                    // Extract request ID
                    let request_id = name
                        .trim_start_matches("request_")
                        .trim_end_matches(".json");

                    // Check if response already exists
                    let response_path = self.messages_dir.join(format!("response_{}.json", request_id));
                    if response_path.exists() {
                        continue;
                    }

                    // Check if request is too old
                    if let Ok(metadata) = entry.metadata() {
                        if let Ok(modified) = metadata.modified() {
                            let age = std::time::SystemTime::now()
                                .duration_since(modified)
                                .unwrap_or_default();
                            
                            if age.as_secs() > timeout_secs {
                                // Remove old request
                                let _ = fs::remove_file(&path);
                                continue;
                            }
                        }
                    }

                    // Read and parse request
                    match self.read_request(&path) {
                        Ok(request) => requests.push(request),
                        Err(e) => eprintln!("Failed to read request {}: {}", path.display(), e),
                    }
                }
            }
        }

        // Sort by priority (lower number = higher priority)
        requests.sort_by_key(|r| r.priority);
        Ok(requests)
    }

    /// Get response for a request (with timeout)
    pub fn get_response(&self, request_id: &str, timeout_secs: u64) -> Result<Option<ExecutionResponse>> {
        let response_path = self.messages_dir.join(format!("response_{}.json", request_id));
        let start = std::time::Instant::now();

        while start.elapsed().as_secs() < timeout_secs {
            if response_path.exists() {
                let response: ExecutionResponse = self.atomic_read(&response_path)?;
                return Ok(Some(response));
            }

            // Check progress file for updates
            let progress_path = self.progress_dir.join(format!("progress_{}.json", request_id));
            if progress_path.exists() {
                if let Ok(progress) = self.atomic_read::<ProgressUpdate>(&progress_path) {
                    eprintln!("Progress: {} - {:?}", progress.stage, progress.message);
                }
            }

            std::thread::sleep(std::time::Duration::from_millis(500));
        }

        Ok(None)
    }

    /// Update progress for a request
    pub fn update_progress(&self, request_id: &str, stage: &str, progress: Option<f32>, message: Option<&str>) -> Result<()> {
        let update = ProgressUpdate {
            request_id: request_id.to_string(),
            timestamp: Utc::now().to_rfc3339(),
            stage: stage.to_string(),
            progress,
            message: message.map(String::from),
        };

        let file_path = self.progress_dir.join(format!("progress_{}.json", request_id));
        self.atomic_write(&file_path, &update)?;
        Ok(())
    }

    /// Check if a request has been cancelled
    pub fn is_cancelled(&self, request_id: &str) -> bool {
        let cancel_path = self.progress_dir.join(format!("cancel_{}.json", request_id));
        cancel_path.exists()
    }

    /// Clean up old messages (older than 1 hour)
    pub fn cleanup_old_messages(&self) -> Result<()> {
        let cutoff_secs = 3600; // 1 hour

        for dir in [&self.messages_dir, &self.progress_dir] {
            if let Ok(entries) = fs::read_dir(dir) {
                for entry in entries.flatten() {
                    if let Ok(metadata) = entry.metadata() {
                        if let Ok(modified) = metadata.modified() {
                            let age = std::time::SystemTime::now()
                                .duration_since(modified)
                                .unwrap_or_default();

                            if age.as_secs() > cutoff_secs {
                                let _ = fs::remove_file(entry.path());
                            }
                        }
                    }
                }
            }
        }

        Ok(())
    }

    /// Atomic write with file locking
    fn atomic_write<T: Serialize>(&self, path: &PathBuf, data: &T) -> Result<()> {
        let temp_path = path.with_extension("tmp");

        // Write to temp file
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&temp_path)?;

        file.lock_exclusive()?;
        let json = serde_json::to_string_pretty(data)?;
        file.write_all(json.as_bytes())?;
        file.unlock()?;

        // Atomic rename
        fs::rename(&temp_path, path)?;
        Ok(())
    }

    /// Atomic read with file locking
    fn atomic_read<T: for<'de> Deserialize<'de>>(&self, path: &PathBuf) -> Result<T> {
        let file = File::open(path)?;
        file.lock_shared()?;

        let mut content = String::new();
        let mut reader = std::io::BufReader::new(&file);
        reader.read_to_string(&mut content)?;

        file.unlock()?;

        let data: T = serde_json::from_str(&content)?;
        Ok(data)
    }

    fn read_request(&self, path: &PathBuf) -> Result<ExecutionRequest> {
        self.atomic_read(path)
    }
}
