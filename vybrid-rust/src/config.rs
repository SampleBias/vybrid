#![allow(dead_code)]

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Daemon lock file data for status checking
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonStatus {
    pub pid: u32,
    pub timestamp: String,
    pub session_id: String,
    pub workers: usize,
}

/// Result of daemon availability check
#[derive(Debug, Clone)]
pub struct DaemonAvailability {
    pub available: bool,
    pub status: Option<DaemonStatus>,
    pub reason: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub api_key: String,
    pub api_base_url: String,
    pub model: String,
    pub vybrid_dir: PathBuf,
    pub messages_dir: PathBuf,
    pub daemon_pool_dir: PathBuf,
    pub progress_dir: PathBuf,
    pub serpapi_key: Option<String>,
}

impl Config {
    pub fn load() -> Result<Self> {
        // 1. First, load global config from ~/.vybrid/.env (if it exists)
        //    This allows vybrid to be launched from any directory
        if let Some(home) = dirs::home_dir() {
            let global_env = home.join(".vybrid").join(".env");
            if global_env.exists() {
                dotenvy::from_path(&global_env).ok();
            }
        }

        // 2. Then load local .env (can override global settings for project-specific config)
        dotenvy::dotenv().ok();

        let api_key = std::env::var("ZAI_API_KEY")
            .or_else(|_| std::env::var("GLM_API_KEY"))
            .context("ZAI_API_KEY or GLM_API_KEY not found in environment. Please set it in your .env file or environment.")?;

        let serpapi_key = std::env::var("SERPAPI_KEY").ok();

        let home = dirs::home_dir().context("Could not find home directory")?;
        let vybrid_dir = home.join(".vybrid");

        // Create all required directories
        std::fs::create_dir_all(&vybrid_dir)
            .context("Failed to create ~/.vybrid directory")?;

        let messages_dir = vybrid_dir.join("messages");
        let daemon_pool_dir = vybrid_dir.join("daemon_pool");
        let progress_dir = vybrid_dir.join("progress");

        std::fs::create_dir_all(&messages_dir)
            .context("Failed to create messages directory")?;
        std::fs::create_dir_all(&daemon_pool_dir)
            .context("Failed to create daemon_pool directory")?;
        std::fs::create_dir_all(&progress_dir)
            .context("Failed to create progress directory")?;

        Ok(Self {
            api_key,
            api_base_url: "https://api.z.ai/api/coding/paas/v4".to_string(),
            model: "glm-4.7".to_string(),
            vybrid_dir,
            messages_dir,
            daemon_pool_dir,
            progress_dir,
            serpapi_key,
        })
    }

    pub fn daemon_lock_file(&self) -> PathBuf {
        self.daemon_pool_dir.join("pool.lock")
    }

    /// Check if the daemon pool is available and running
    pub fn check_daemon_availability(&self) -> DaemonAvailability {
        let lock_file = self.daemon_lock_file();

        // Check if lock file exists
        if !lock_file.exists() {
            return DaemonAvailability {
                available: false,
                status: None,
                reason: Some("Daemon lock file does not exist. Daemon is not running.".to_string()),
            };
        }

        // Try to read and parse lock file
        let content = match std::fs::read_to_string(&lock_file) {
            Ok(c) => c,
            Err(e) => {
                return DaemonAvailability {
                    available: false,
                    status: None,
                    reason: Some(format!("Failed to read daemon lock file: {}", e)),
                };
            }
        };

        let status: DaemonStatus = match serde_json::from_str(&content) {
            Ok(s) => s,
            Err(e) => {
                return DaemonAvailability {
                    available: false,
                    status: None,
                    reason: Some(format!("Failed to parse daemon lock file: {}", e)),
                };
            }
        };

        // Check if timestamp is stale (older than 10 minutes)
        if let Ok(timestamp) = chrono::DateTime::parse_from_rfc3339(&status.timestamp) {
            let age = chrono::Utc::now().signed_duration_since(timestamp);
            if age.num_minutes() > 10 {
                return DaemonAvailability {
                    available: false,
                    status: Some(status),
                    reason: Some("Daemon lock file is stale (>10 minutes old).".to_string()),
                };
            }
        }

        // Check if process is still alive (Linux-specific)
        let proc_path = PathBuf::from(format!("/proc/{}", status.pid));
        if !proc_path.exists() {
            let pid = status.pid;
            return DaemonAvailability {
                available: false,
                status: Some(status),
                reason: Some(format!("Daemon process (PID {}) is no longer running.", pid)),
            };
        }

        // All checks passed - daemon is available
        DaemonAvailability {
            available: true,
            status: Some(status),
            reason: None,
        }
    }

    /// Quick check if daemon is available (returns bool only)
    pub fn is_daemon_available(&self) -> bool {
        self.check_daemon_availability().available
    }
}
