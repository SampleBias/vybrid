#![allow(dead_code)]

use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};

/// Default OpenAI-compatible base for [LM Studio](https://lmstudio.ai/docs/developer/openai-compat).
pub const DEFAULT_LM_STUDIO_BASE_URL: &str = "http://127.0.0.1:1234/v1";

/// Placeholder Bearer token when LM Studio has authentication disabled.
pub const DEFAULT_LM_STUDIO_API_KEY: &str = "lm-studio";
pub const DEFAULT_RUST_LSP_COMMAND: &str = "rust-analyzer";

const GROQ_BASE_URL: &str = "https://api.groq.com/openai/v1";
pub const DEFAULT_GROQ_RATE_LIMIT_FALLBACK_MODEL: &str = "qwen/qwen3-32b";
pub const DEFAULT_GROQ_CONTEXT_TOKEN_BUDGET: u32 = 36_000;
pub const DEFAULT_GROQ_RETRY_CONTEXT_TOKEN_BUDGET: u32 = 18_000;
pub const DEFAULT_MAX_COMPLETION_TOKENS: u32 = 4_096;

/// Which LLM backend Vybrid uses (`VYBRID_LLM_PROVIDER`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LlmProvider {
    Groq,
    LmStudio,
}

impl LlmProvider {
    /// Parse `groq` or `lmstudio` (case-insensitive). Returns `None` if unknown.
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "groq" => Some(Self::Groq),
            "lmstudio" | "lm_studio" | "lm-studio" => Some(Self::LmStudio),
            _ => None,
        }
    }

    pub fn as_env_value(self) -> &'static str {
        match self {
            LlmProvider::Groq => "groq",
            LlmProvider::LmStudio => "lmstudio",
        }
    }
}

/// Path to the project `.env` file: `vybrid-rust/.env` (directory containing `Cargo.toml`).
/// Override with `VYBRID_ROOT` if the binary was moved and keys live elsewhere.
pub fn project_env_file_path() -> PathBuf {
    if let Ok(root) = std::env::var("VYBRID_ROOT") {
        PathBuf::from(root).join(".env")
    } else {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".env")
    }
}

fn normalize_openai_base_url(url: &str) -> String {
    url.trim().trim_end_matches('/').to_string()
}

#[derive(Debug, Clone)]
pub struct Config {
    pub llm_provider: LlmProvider,
    pub groq_api_key: Option<String>,
    pub groq_model: String,
    pub groq_rate_limit_fallback_model: String,
    pub lm_studio_base_url: String,
    pub lm_studio_api_key: Option<String>,
    pub lm_studio_model: Option<String>,
    /// `vybrid-rust/.env` (see [`project_env_file_path`]).
    pub env_file_path: PathBuf,
    /// `~/.vybrid/.env` — mirror so launches from any directory find keys without `VYBRID_ROOT`.
    pub global_env_file_path: PathBuf,
    pub vybrid_dir: PathBuf,
    pub messages_dir: PathBuf,
    pub progress_dir: PathBuf,
    pub serpapi_key: Option<String>,
    pub rust_lsp_enabled: bool,
    pub rust_lsp_command: String,
    pub rust_lsp_root: Option<PathBuf>,
    pub context_token_budget: u32,
    pub retry_context_token_budget: u32,
    pub max_completion_tokens: u32,
}

impl Config {
    pub fn load() -> Result<Self> {
        let home = dirs::home_dir().context("Could not find home directory")?;
        let vybrid_dir = home.join(".vybrid");

        // Create all required directories
        std::fs::create_dir_all(&vybrid_dir).context("Failed to create ~/.vybrid directory")?;

        let messages_dir = vybrid_dir.join("messages");
        let progress_dir = vybrid_dir.join("progress");

        std::fs::create_dir_all(&messages_dir).context("Failed to create messages directory")?;
        std::fs::create_dir_all(&progress_dir).context("Failed to create progress directory")?;

        let global_env_file_path = vybrid_dir.join(".env");
        let env_file_path = project_env_file_path();

        // Load global first, then project — project overrides for duplicate keys.
        dotenvy::from_path(&global_env_file_path).ok();
        dotenvy::from_path(&env_file_path).ok();

        let llm_provider = std::env::var("VYBRID_LLM_PROVIDER")
            .ok()
            .and_then(|s| LlmProvider::parse(&s))
            .unwrap_or(LlmProvider::Groq);

        let groq_api_key = std::env::var("GROQ_API_KEY").ok();

        let serpapi_key = std::env::var("SERPAPI_KEY").ok();

        let groq_model =
            std::env::var("GROQ_MODEL").unwrap_or_else(|_| "openai/gpt-oss-120b".to_string());
        let groq_rate_limit_fallback_model = std::env::var("GROQ_RATE_LIMIT_FALLBACK_MODEL")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| DEFAULT_GROQ_RATE_LIMIT_FALLBACK_MODEL.to_string());

        let lm_studio_base_url = std::env::var("LM_STUDIO_BASE_URL")
            .ok()
            .map(|s| normalize_openai_base_url(&s))
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| normalize_openai_base_url(DEFAULT_LM_STUDIO_BASE_URL));

        let lm_studio_api_key = std::env::var("LM_STUDIO_API_KEY")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());

        let lm_studio_model = std::env::var("LM_STUDIO_MODEL")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());

        let rust_lsp_enabled = std::env::var("VYBRID_RUST_LSP_ENABLED")
            .ok()
            .map(|s| parse_bool_env(&s))
            .unwrap_or(false);
        let rust_lsp_command = std::env::var("VYBRID_RUST_LSP_COMMAND")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| DEFAULT_RUST_LSP_COMMAND.to_string());
        let rust_lsp_root = std::env::var("VYBRID_RUST_LSP_ROOT")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .map(PathBuf::from);
        let context_token_budget = parse_u32_env(
            "VYBRID_GROQ_CONTEXT_TOKEN_BUDGET",
            DEFAULT_GROQ_CONTEXT_TOKEN_BUDGET,
        );
        let retry_context_token_budget = parse_u32_env(
            "VYBRID_GROQ_RETRY_CONTEXT_TOKEN_BUDGET",
            DEFAULT_GROQ_RETRY_CONTEXT_TOKEN_BUDGET,
        );
        let max_completion_tokens = parse_u32_env(
            "VYBRID_MAX_COMPLETION_TOKENS",
            DEFAULT_MAX_COMPLETION_TOKENS,
        );

        Ok(Self {
            llm_provider,
            groq_api_key,
            groq_model,
            groq_rate_limit_fallback_model,
            lm_studio_base_url,
            lm_studio_api_key,
            lm_studio_model,
            env_file_path,
            global_env_file_path,
            vybrid_dir,
            messages_dir,
            progress_dir,
            serpapi_key,
            rust_lsp_enabled,
            rust_lsp_command,
            rust_lsp_root,
            context_token_budget,
            retry_context_token_budget,
            max_completion_tokens,
        })
    }

    /// Resolves `(api_key, base_url, model)` for the active [`LlmProvider`] for OpenAI-compatible chat.
    pub fn effective_chat_client_params(&self) -> Option<(String, String, String)> {
        match self.llm_provider {
            LlmProvider::Groq => {
                let key = self.groq_api_key.clone()?;
                if key.trim().is_empty() {
                    return None;
                }
                Some((key, GROQ_BASE_URL.to_string(), self.groq_model.clone()))
            }
            LlmProvider::LmStudio => {
                let model = self.lm_studio_model.as_ref()?.trim();
                if model.is_empty() {
                    return None;
                }
                let key = self
                    .lm_studio_api_key
                    .as_ref()
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| DEFAULT_LM_STUDIO_API_KEY.to_string());
                Some((key, self.lm_studio_base_url.clone(), model.to_string()))
            }
        }
    }

    /// Writes the same key to `~/.vybrid/.env` and `vybrid-rust/.env` so use from any cwd works.
    fn persist_env_key(&self, key: &str, value: &str) -> Result<()> {
        merge_env_file(&self.global_env_file_path, key, value)?;
        merge_env_file(&self.env_file_path, key, value)?;
        Ok(())
    }

    /// Writes or updates `GROQ_API_KEY` in both env files and switches provider to Groq.
    pub fn set_groq_api_key(&mut self, key: String) -> Result<()> {
        self.persist_env_key("GROQ_API_KEY", &key)?;
        std::env::set_var("GROQ_API_KEY", &key);
        self.groq_api_key = Some(key);
        self.set_llm_provider(LlmProvider::Groq)?;
        Ok(())
    }

    /// Writes or updates `VYBRID_LLM_PROVIDER` (`groq` or `lmstudio`).
    pub fn set_llm_provider(&mut self, provider: LlmProvider) -> Result<()> {
        let v = provider.as_env_value();
        self.persist_env_key("VYBRID_LLM_PROVIDER", v)?;
        std::env::set_var("VYBRID_LLM_PROVIDER", v);
        self.llm_provider = provider;
        Ok(())
    }

    pub fn set_lm_studio_base_url(&mut self, url: String) -> Result<()> {
        let normalized = normalize_openai_base_url(&url);
        if normalized.is_empty() {
            anyhow::bail!("LM Studio base URL was empty.");
        }
        self.persist_env_key("LM_STUDIO_BASE_URL", &normalized)?;
        std::env::set_var("LM_STUDIO_BASE_URL", &normalized);
        self.lm_studio_base_url = normalized;
        Ok(())
    }

    /// Persists the API key (Bearer token). Use [`DEFAULT_LM_STUDIO_API_KEY`] when the server has auth disabled.
    pub fn set_lm_studio_api_key(&mut self, key: String) -> Result<()> {
        self.persist_env_key("LM_STUDIO_API_KEY", &key)?;
        std::env::set_var("LM_STUDIO_API_KEY", &key);
        self.lm_studio_api_key = Some(key);
        Ok(())
    }

    pub fn set_lm_studio_model(&mut self, model: String) -> Result<()> {
        let trimmed = model.trim().to_string();
        if trimmed.is_empty() {
            anyhow::bail!("LM Studio model id was empty.");
        }
        self.persist_env_key("LM_STUDIO_MODEL", &trimmed)?;
        std::env::set_var("LM_STUDIO_MODEL", &trimmed);
        self.lm_studio_model = Some(trimmed);
        Ok(())
    }

    /// Writes LM Studio settings and selects the LM Studio provider.
    pub fn apply_lm_studio_profile(
        &mut self,
        base_url: String,
        api_key: String,
        model: String,
    ) -> Result<()> {
        let base = normalize_openai_base_url(&base_url);
        if base.is_empty() {
            anyhow::bail!("LM Studio base URL was empty.");
        }
        let model_trim = model.trim().to_string();
        if model_trim.is_empty() {
            anyhow::bail!("LM Studio model id was empty.");
        }
        let key = if api_key.trim().is_empty() {
            DEFAULT_LM_STUDIO_API_KEY.to_string()
        } else {
            api_key.trim().to_string()
        };

        self.persist_env_key("LM_STUDIO_BASE_URL", &base)?;
        self.persist_env_key("LM_STUDIO_API_KEY", &key)?;
        self.persist_env_key("LM_STUDIO_MODEL", &model_trim)?;
        std::env::set_var("LM_STUDIO_BASE_URL", &base);
        std::env::set_var("LM_STUDIO_API_KEY", &key);
        std::env::set_var("LM_STUDIO_MODEL", &model_trim);

        self.lm_studio_base_url = base;
        self.lm_studio_api_key = Some(key);
        self.lm_studio_model = Some(model_trim);

        self.set_llm_provider(LlmProvider::LmStudio)?;
        Ok(())
    }

    /// Writes or updates `SERPAPI_KEY` in both env files.
    pub fn set_serpapi_key(&mut self, key: String) -> Result<()> {
        self.persist_env_key("SERPAPI_KEY", &key)?;
        std::env::set_var("SERPAPI_KEY", &key);
        self.serpapi_key = Some(key);
        Ok(())
    }

    pub fn set_rust_lsp_enabled(&mut self, enabled: bool) -> Result<()> {
        let value = if enabled { "true" } else { "false" };
        self.persist_env_key("VYBRID_RUST_LSP_ENABLED", value)?;
        std::env::set_var("VYBRID_RUST_LSP_ENABLED", value);
        self.rust_lsp_enabled = enabled;
        Ok(())
    }

    pub fn set_rust_lsp_command(&mut self, command: String) -> Result<()> {
        let command = command.trim().to_string();
        if command.is_empty() {
            anyhow::bail!("Rust LSP command was empty.");
        }
        self.persist_env_key("VYBRID_RUST_LSP_COMMAND", &command)?;
        std::env::set_var("VYBRID_RUST_LSP_COMMAND", &command);
        self.rust_lsp_command = command;
        Ok(())
    }

    pub fn set_rust_lsp_root(&mut self, root: Option<PathBuf>) -> Result<()> {
        let value = root
            .as_ref()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();
        self.persist_env_key("VYBRID_RUST_LSP_ROOT", &value)?;
        if value.is_empty() {
            std::env::remove_var("VYBRID_RUST_LSP_ROOT");
        } else {
            std::env::set_var("VYBRID_RUST_LSP_ROOT", &value);
        }
        self.rust_lsp_root = root;
        Ok(())
    }
}

fn parse_bool_env(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

fn parse_u32_env(key: &str, default: u32) -> u32 {
    std::env::var(key)
        .ok()
        .and_then(|s| s.trim().parse::<u32>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(default)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_env_value_quotes_spaces_and_hashes() {
        assert_eq!(format_env_value("plain-token"), "plain-token");
        assert_eq!(format_env_value("has space"), "\"has space\"");
        assert_eq!(format_env_value("has#hash"), "\"has#hash\"");
    }

    #[test]
    fn merge_env_file_replaces_existing_key() {
        let path = std::env::temp_dir().join(format!(
            "vybrid-env-{}-{}.env",
            std::process::id(),
            "replace"
        ));
        let _ = std::fs::remove_file(&path);
        std::fs::write(&path, "A=1\nTARGET=old\n# comment\n").unwrap();

        merge_env_file(&path, "TARGET", "new value").unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        let _ = std::fs::remove_file(&path);

        assert!(content.contains("TARGET=\"new value\""));
        assert!(!content.contains("TARGET=old"));
        assert!(content.contains("# comment"));
    }
}
