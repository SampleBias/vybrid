#![allow(dead_code)]

use anyhow::{Context, Result};
use console::style;
use dialoguer::{Input, Select};

use crate::client::groq::GroqClient;
use crate::client::openrouter::{
    clear_model_cache, fetch_models, format_model_label, ModelQuery, POPULAR_PROVIDERS,
};
use crate::config::{Config, DEFAULT_LM_STUDIO_BASE_URL, LlmProvider};
use crate::lsp::RustLspManager;
use crate::ui;

const MENU_PAGE_SIZE: usize = 8;
const MODEL_PAGE_SIZE: usize = 12;

fn saved_env_locations(config: &Config) -> String {
    format!(
        "{}\n  {}",
        config.global_env_file_path.display(),
        config.env_file_path.display()
    )
}

pub(crate) fn resolve_rust_lsp_root(config: &Config) -> Result<std::path::PathBuf> {
    let root = config
        .rust_lsp_root
        .clone()
        .unwrap_or(std::env::current_dir().context("Could not resolve current directory")?);
    if root.is_absolute() {
        Ok(root)
    } else {
        Ok(std::env::current_dir()
            .context("Could not resolve current directory")?
            .join(root))
    }
}

/// Interactive setup menu — keys written to `~/.vybrid/.env` and `vybrid-rust/.env`.
pub async fn handle_menu(
    config: &mut Config,
    client: &mut Option<GroqClient>,
    rust_lsp: &RustLspManager,
) -> Result<()> {
    let items = vec![
        "Groq (cloud)",
        "OpenRouter (multi-provider models)",
        "LM Studio (local)",
        "SerpAPI (Google search)",
        "Rust LSP (rust-analyzer)",
        "Back",
    ];
    let sel = Select::new()
        .with_prompt("Vybrid menu")
        .items(&items)
        .max_length(MENU_PAGE_SIZE)
        .default(0)
        .interact()
        .context("Menu cancelled")?;

    match sel {
        0 => handle_groq_menu(config, client).await?,
        1 => handle_openrouter_menu(config, client).await?,
        2 => handle_lm_studio_menu(config, client).await?,
        3 => handle_serpapi_menu(config).await?,
        4 => handle_rust_lsp_menu(config, rust_lsp).await?,
        _ => {}
    }
    Ok(())
}

async fn handle_groq_menu(config: &mut Config, client: &mut Option<GroqClient>) -> Result<()> {
    loop {
        let items = vec![
            "Add Groq + optional SerpAPI keys (quick setup)",
            "Add or update Groq API key only",
            "Switch to Groq",
            "Back",
        ];
        let sel = Select::new()
            .with_prompt("Groq")
            .items(&items)
            .max_length(MENU_PAGE_SIZE)
            .default(0)
            .interact()
            .context("Groq menu cancelled")?;

        match sel {
            0 => {
                let key: String = Input::new()
                    .with_prompt("Groq API key")
                    .interact_text()
                    .context("No API key entered")?;
                let key = key.trim().to_string();
                if key.is_empty() {
                    ui::print_error("Groq API key was empty.");
                    continue;
                }
                config.set_groq_api_key(key)?;
                *client = config.build_chat_client();

                let serp: String = Input::new()
                    .with_prompt("SerpAPI key (optional — Enter to skip)")
                    .allow_empty(true)
                    .interact_text()
                    .context("SerpAPI prompt failed")?;
                let serp = serp.trim();
                if !serp.is_empty() {
                    config.set_serpapi_key(serp.to_string())?;
                }

                println!(
                    "{}",
                    style(format!(
                        "Saved key(s) — you can start chatting. Files updated:\n  {}",
                        saved_env_locations(config)
                    ))
                    .green()
                );
            }
            1 => {
                let key: String = Input::new()
                    .with_prompt("Groq API key")
                    .interact_text()
                    .context("No API key entered")?;
                let key = key.trim().to_string();
                if key.is_empty() {
                    ui::print_error("API key was empty.");
                    continue;
                }
                config.set_groq_api_key(key)?;
                *client = config.build_chat_client();
                println!(
                    "{}",
                    style(format!(
                        "Saved GROQ_API_KEY to:\n  {}",
                        saved_env_locations(config)
                    ))
                    .green()
                );
            }
            2 => {
                config.set_llm_provider(LlmProvider::Groq)?;
                *client = config.build_chat_client();
                if client.is_some() {
                    println!(
                        "{}",
                        style(format!(
                            "Switched to Groq. VYBRID_LLM_PROVIDER=groq — settings:\n  {}",
                            saved_env_locations(config)
                        ))
                        .green()
                    );
                } else {
                    ui::print_error(
                        "VYBRID_LLM_PROVIDER is now groq, but GROQ_API_KEY is missing.",
                    );
                }
            }
            _ => break,
        }
    }
    Ok(())
}

async fn handle_openrouter_menu(
    config: &mut Config,
    client: &mut Option<GroqClient>,
) -> Result<()> {
    loop {
        let items = vec![
            "Add or update OpenRouter API key",
            "Select model",
            "Switch to OpenRouter",
            "Refresh model catalog",
            "Back",
        ];
        let sel = Select::new()
            .with_prompt(format!(
                "OpenRouter (current: {})",
                config.openrouter_model
            ))
            .items(&items)
            .max_length(MENU_PAGE_SIZE)
            .default(0)
            .interact()
            .context("OpenRouter menu cancelled")?;

        match sel {
            0 => {
                let key: String = Input::new()
                    .with_prompt("OpenRouter API key")
                    .interact_text()
                    .context("No API key entered")?;
                config.set_openrouter_api_key(key)?;
                println!(
                    "{}",
                    style(format!(
                        "Saved OPENROUTER_API_KEY to:\n  {}",
                        saved_env_locations(config)
                    ))
                    .green()
                );
                println!(
                    "{}",
                    style("Pick a model next (Select model) before chatting.").dim()
                );
            }
            1 => {
                if config.openrouter_api_key.as_deref().is_none_or(|k| k.trim().is_empty()) {
                    ui::print_error("Add an OpenRouter API key first.");
                    continue;
                }
                if let Some(model_id) = handle_openrouter_model_picker(config).await? {
                    config.set_openrouter_model(model_id)?;
                    *client = config.build_chat_client();
                    println!(
                        "{}",
                        style(format!(
                            "Saved OPENROUTER_MODEL — settings:\n  {}",
                            saved_env_locations(config)
                        ))
                        .green()
                    );
                }
            }
            2 => {
                config.set_llm_provider(LlmProvider::OpenRouter)?;
                *client = config.build_chat_client();
                if client.is_some() {
                    println!(
                        "{}",
                        style(format!(
                            "Switched to OpenRouter ({}) — settings:\n  {}",
                            config.openrouter_model,
                            saved_env_locations(config)
                        ))
                        .green()
                    );
                } else {
                    ui::print_error(
                        "OpenRouter is selected but OPENROUTER_API_KEY or OPENROUTER_MODEL is missing.",
                    );
                }
            }
            3 => {
                clear_model_cache()?;
                println!("{}", style("OpenRouter model cache cleared.").green());
            }
            _ => break,
        }
    }
    Ok(())
}

async fn handle_openrouter_model_picker(config: &Config) -> Result<Option<String>> {
    let api_key = config
        .openrouter_api_key
        .as_deref()
        .context("OpenRouter API key missing")?;

    loop {
        let items = vec![
            "Recommended for coding (popular, tool-capable)",
            "Search by name",
            "Browse by provider",
            "Advanced: full catalog",
            "Back",
        ];
        let sel = Select::new()
            .with_prompt("How do you want to pick a model?")
            .items(&items)
            .max_length(MENU_PAGE_SIZE)
            .default(0)
            .interact()
            .context("Model browse cancelled")?;

        let query = match sel {
            0 => ModelQuery::Recommended,
            1 => {
                let term: String = Input::new()
                    .with_prompt("Search term (e.g. claude, gpt-4, qwen)")
                    .interact_text()
                    .context("Search prompt failed")?;
                let term = term.trim().to_string();
                if term.is_empty() {
                    ui::print_error("Search term was empty.");
                    continue;
                }
                ModelQuery::Search(term)
            }
            2 => {
                let provider_labels: Vec<String> = POPULAR_PROVIDERS
                    .iter()
                    .map(|(_, label)| label.to_string())
                    .collect();
                let psel = Select::new()
                    .with_prompt("Provider")
                    .items(&provider_labels)
                    .max_length(MENU_PAGE_SIZE)
                    .default(0)
                    .interact()
                    .context("Provider selection cancelled")?;
                let slug = POPULAR_PROVIDERS[psel].0.to_string();
                ModelQuery::Provider(slug)
            }
            3 => {
                println!(
                    "{}",
                    style(
                        "Warning: models without tool support may not work with Vybrid's coding agent."
                    )
                    .yellow()
                );
                ModelQuery::FullCatalog
            }
            _ => return Ok(None),
        };

        if let Some(model_id) = pick_model_from_query(api_key, &query).await? {
            return Ok(Some(model_id));
        }
    }
}

async fn pick_model_from_query(api_key: &str, query: &ModelQuery) -> Result<Option<String>> {
    println!("{}", style("Fetching models from OpenRouter…").dim());
    let models = fetch_models(api_key, query, false).await?;

    if models.is_empty() {
        ui::print_error("No models matched. Try a different search or browse mode.");
        return Ok(None);
    }

    let show_no_tools = matches!(query, ModelQuery::FullCatalog);
    let labels: Vec<String> = models
        .iter()
        .map(|m| format_model_label(m, show_no_tools))
        .collect();

    let mut default = 0usize;
    if let ModelQuery::Recommended = query {
        // Keep default at top (most popular).
        default = 0;
    }

    let sel = Select::new()
        .with_prompt(format!("Select model ({} found)", models.len()))
        .items(&labels)
        .max_length(MODEL_PAGE_SIZE)
        .default(default)
        .interact()
        .context("Model selection cancelled")?;

    Ok(Some(models[sel].id.clone()))
}

async fn handle_lm_studio_menu(
    config: &mut Config,
    client: &mut Option<GroqClient>,
) -> Result<()> {
    let default_base = DEFAULT_LM_STUDIO_BASE_URL;
    let base_raw: String = Input::new()
        .with_prompt(format!(
            "LM Studio OpenAI base URL (Enter for {default_base})"
        ))
        .allow_empty(true)
        .interact_text()
        .context("Base URL prompt failed")?;
    let base = if base_raw.trim().is_empty() {
        default_base.to_string()
    } else {
        base_raw.trim().to_string()
    };
    let api_key: String = Input::new()
        .with_prompt("LM Studio API key (empty = placeholder when auth is off)")
        .allow_empty(true)
        .interact_text()
        .context("API key prompt failed")?;
    let model: String = Input::new()
        .with_prompt("Model id (must match the model loaded in LM Studio)")
        .interact_text()
        .context("Model id required")?;
    let model = model.trim().to_string();
    if model.is_empty() {
        ui::print_error("Model id was empty.");
        return Ok(());
    }
    config.apply_lm_studio_profile(base, api_key, model)?;
    *client = config.build_chat_client();
    println!(
        "{}",
        style(format!(
            "Saved LM Studio profile (VYBRID_LLM_PROVIDER=lmstudio) to:\n  {}",
            saved_env_locations(config)
        ))
        .green()
    );
    Ok(())
}

async fn handle_serpapi_menu(config: &mut Config) -> Result<()> {
    let key: String = Input::new()
        .with_prompt("SerpAPI key")
        .interact_text()
        .context("No API key entered")?;
    let key = key.trim().to_string();
    if key.is_empty() {
        ui::print_error("SerpAPI key was empty.");
        return Ok(());
    }
    config.set_serpapi_key(key)?;
    println!(
        "{}",
        style(format!(
            "Saved SERPAPI_KEY to:\n  {}",
            saved_env_locations(config)
        ))
        .green()
    );
    Ok(())
}

async fn handle_rust_lsp_menu(config: &mut Config, rust_lsp: &RustLspManager) -> Result<()> {
    loop {
        let status = rust_lsp.status().await;
        let items = vec![
            "Connect now",
            "Disconnect",
            "Restart",
            "Show status",
            if config.rust_lsp_enabled {
                "Disable auto-connect"
            } else {
                "Enable auto-connect"
            },
            "Configure rust-analyzer command",
            "Configure workspace root",
            "Back",
        ];
        let sel = Select::new()
            .with_prompt(format!(
                "Rust LSP menu ({})",
                status.summary().lines().next().unwrap_or("Rust LSP")
            ))
            .items(&items)
            .max_length(MENU_PAGE_SIZE)
            .default(0)
            .interact()
            .context("Rust LSP menu cancelled")?;

        match sel {
            0 => {
                let root = resolve_rust_lsp_root(config)?;
                rust_lsp.connect(&config.rust_lsp_command, root).await?;
                println!("{}", style("Rust LSP connected.").green());
            }
            1 => {
                rust_lsp.disconnect().await?;
                println!("{}", style("Rust LSP disconnected.").green());
            }
            2 => {
                let root = resolve_rust_lsp_root(config)?;
                rust_lsp.restart(&config.rust_lsp_command, root).await?;
                println!("{}", style("Rust LSP restarted.").green());
            }
            3 => {
                println!("{}", style(rust_lsp.status().await.summary()).dim());
            }
            4 => {
                let enabled = !config.rust_lsp_enabled;
                config.set_rust_lsp_enabled(enabled)?;
                if enabled {
                    let root = resolve_rust_lsp_root(config)?;
                    match rust_lsp.connect(&config.rust_lsp_command, root).await {
                        Ok(()) => println!("{}", style("Rust LSP auto-connect enabled.").green()),
                        Err(e) => ui::print_error(&format!(
                            "Auto-connect enabled, but connection failed: {}",
                            e
                        )),
                    }
                } else {
                    rust_lsp.disconnect().await?;
                    println!("{}", style("Rust LSP auto-connect disabled.").green());
                }
            }
            5 => {
                let command: String = Input::new()
                    .with_prompt("Rust LSP command")
                    .default(config.rust_lsp_command.clone())
                    .interact_text()
                    .context("Rust LSP command prompt failed")?;
                config.set_rust_lsp_command(command)?;
                println!(
                    "{}",
                    style(format!(
                        "Saved VYBRID_RUST_LSP_COMMAND to:\n  {}",
                        saved_env_locations(config)
                    ))
                    .green()
                );
            }
            6 => {
                let current = config
                    .rust_lsp_root
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default();
                let root: String = Input::new()
                    .with_prompt("Workspace root (empty = current directory at runtime)")
                    .default(current)
                    .allow_empty(true)
                    .interact_text()
                    .context("Rust LSP root prompt failed")?;
                let root = root.trim();
                if root.is_empty() {
                    config.set_rust_lsp_root(None)?;
                } else {
                    config.set_rust_lsp_root(Some(std::path::PathBuf::from(root)))?;
                }
                println!(
                    "{}",
                    style(format!(
                        "Saved VYBRID_RUST_LSP_ROOT to:\n  {}",
                        saved_env_locations(config)
                    ))
                    .green()
                );
            }
            _ => break,
        }
    }
    Ok(())
}
