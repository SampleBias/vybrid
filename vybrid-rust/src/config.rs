#![allow(dead_code)]

use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct Config {
    pub api_key: Option<String>,
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
            .ok();

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
            model: "glm-5.1".to_string(),
            vybrid_dir,
            messages_dir,
            progress_dir,
            serpapi_key,
        })
    }

    /// Writes or updates `ZAI_API_KEY` in `~/.vybrid/.env` so it applies in every directory (local `.env` can still override).
    pub fn set_zai_api_key_global(&mut self, key: String) -> Result<()> {
        let path = self.vybrid_dir.join(".env");
        merge_env_file(&path, "ZAI_API_KEY", &key)?;
        std::env::set_var("ZAI_API_KEY", &key);
        self.api_key = Some(key);
        Ok(())
    }
}

fn format_env_value(value: &str) -> String {
    if value.is_empty() {
        return String::new();
    }
    if value
        .chars()
        .any(|c| c.is_whitespace() || c == '#' || c == '"' || c == '\'')
    {
        format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
    } else {
        value.to_string()
    }
}

fn merge_env_file(path: &Path, key: &str, value: &str) -> Result<()> {
    let formatted_value = format_env_value(value);
    let new_line = format!("{}={}", key, formatted_value);

    let mut lines: Vec<String> = Vec::new();
    let mut replaced = false;

    if path.exists() {
        let content = fs::read_to_string(path)
            .with_context(|| format!("Failed to read {}", path.display()))?;
        for line in content.lines() {
            let trimmed = line.trim_start();
            if trimmed.starts_with('#') || trimmed.is_empty() {
                lines.push(line.to_string());
                continue;
            }
            if let Some(eq_pos) = line.find('=') {
                let k = line[..eq_pos].trim();
                if k == key {
                    lines.push(new_line.clone());
                    replaced = true;
                    continue;
                }
            }
            lines.push(line.to_string());
        }
    }

    if !replaced {
        if !lines.is_empty()
            && !lines
                .last()
                .map(|l| l.as_str().trim().is_empty())
                .unwrap_or(true)
        {
            lines.push(String::new());
        }
        lines.push(new_line);
    }

    let out = lines.join("\n") + "\n";
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create {}", parent.display()))?;
        }
    }
    fs::write(path, out).with_context(|| format!("Failed to write {}", path.display()))?;
    Ok(())
}
