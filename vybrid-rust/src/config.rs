#![allow(dead_code)]

use anyhow::{Context, Result};
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct Config {
    pub api_key: String,
    pub api_base_url: String,
    pub model: String,
    pub vybrid_dir: PathBuf,
    pub messages_dir: PathBuf,
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
        let progress_dir = vybrid_dir.join("progress");

        std::fs::create_dir_all(&messages_dir)
            .context("Failed to create messages directory")?;
        std::fs::create_dir_all(&progress_dir)
            .context("Failed to create progress directory")?;

        Ok(Self {
            api_key,
            api_base_url: "https://api.z.ai/api/coding/paas/v4".to_string(),
            model: "glm-4.7".to_string(),
            vybrid_dir,
            messages_dir,
            progress_dir,
            serpapi_key,
        })
    }
}
