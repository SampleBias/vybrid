#![allow(dead_code)]

use anyhow::{Context, Result};
use console::style;
use dialoguer::{Input, Password, Select};

use crate::client::groq::GroqClient;
use crate::client::groq_models::{self, ModelQuery as GroqModelQuery};
use crate::client::openrouter::{
    clear_model_cache, fetch_models, format_model_label, ModelQuery, OpenRouterModel,
    POPULAR_PROVIDERS,
};
use crate::config::{Config, LlmProvider, DEFAULT_LM_STUDIO_BASE_URL};
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
            "Add or update Groq API key",
            "Switch to Groq",
            "Select model",
            "Advanced settings",
            "Back",
        ];
        let sel = Select::new()
            .with_prompt(format!(
                "Groq (current: {}, jev {})",
                config.groq_model,
                if config.groq_jev_routing { "on" } else { "off" }
            ))
            .items(&items)
            .max_length(MENU_PAGE_SIZE)
            .default(0)
            .interact()
            .context("Groq menu cancelled")?;

        let sel = if sel == 3 {
            let advanced = vec![
                format!(
                    "Turn Jev routing {}",
                    if config.groq_jev_routing { "off" } else { "on" }
                ),
                "Choose Jev tier models".to_string(),
                "Refresh model catalog".to_string(),
                "Add or update OpenRouter key for Jev decisions".to_string(),
                "Back".to_string(),
            ];
            let choice = Select::new()
                .with_prompt("Groq advanced settings")
                .items(&advanced)
                .default(0)
                .interact()
                .context("Groq advanced menu cancelled")?;
            if choice == 4 {
                continue;
            }
            choice + 4
        } else if sel == 4 {
            break;
        } else {
            sel + 1
        };

        match sel {
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
            3 => match handle_groq_model_picker(config, &config.groq_model).await {
                Ok(Some(model)) => {
                    config.set_groq_model(model)?;
                    *client = config.build_chat_client();
                    println!(
                        "{}",
                        style(format!(
                            "Saved GROQ_MODEL={} — settings:\n  {}",
                            config.groq_model,
                            saved_env_locations(config)
                        ))
                        .green()
                    );
                }
                Ok(None) => {}
                Err(error) => ui::print_error(&error.to_string()),
            },
            4 => {
                if !config.groq_jev_routing
                    && config
                        .openrouter_api_key
                        .as_deref()
                        .is_none_or(|k| k.trim().is_empty())
                {
                    ui::print_error("Jev needs an OpenRouter API key for decisions. Use 'Add or update OpenRouter key for Jev decisions'. Chat will still use your Groq key.");
                    continue;
                }
                config.set_groq_jev_routing(!config.groq_jev_routing)?;
                println!("{}", style(format!("Groq Jev routing {}. Jev decisions use OpenRouter; chat uses Groq. {} is the pinned fallback.",
                    if config.groq_jev_routing { "on" } else { "off" }, config.groq_model)).green());
            }
            5 => {
                if let Err(error) = handle_groq_jev_tiers(config).await {
                    ui::print_error(&error.to_string());
                }
            }
            6 => {
                match groq_models::fetch_models(config.groq_api_key.as_deref().unwrap_or(""), true)
                    .await
                {
                    Ok(models) => println!(
                        "{}",
                        style(format!("Groq catalog refreshed ({} models).", models.len())).green()
                    ),
                    Err(error) => ui::print_error(&error.to_string()),
                }
            }
            7 => {
                let key = Password::new()
                    .with_prompt("OpenRouter API key (Jev decisions only; billed to OpenRouter)")
                    .interact()
                    .context("No OpenRouter key entered")?;
                config.set_openrouter_api_key(key)?;
                *client = config.build_chat_client();
                println!(
                    "{}",
                    style("Saved OpenRouter key for Jev decisions. Groq chat uses GROQ_API_KEY.")
                        .green()
                );
            }
            _ => break,
        }
    }
    Ok(())
}

async fn handle_groq_model_picker(config: &Config, current: &str) -> Result<Option<String>> {
    println!("{}", style("Fetching models from Groq…").dim());
    let models =
        groq_models::fetch_models(config.groq_api_key.as_deref().unwrap_or(""), false).await?;
    loop {
        let mode = Select::new()
            .with_prompt("Choose a Groq model")
            .items(&[
                "Recommended for coding",
                "Search by name",
                "Browse by model author",
                "Advanced: full catalog",
                "Back",
            ])
            .max_length(MENU_PAGE_SIZE)
            .interact_opt()?;
        let query = match mode {
            Some(0) => GroqModelQuery::Recommended,
            Some(1) => {
                let term: String = Input::new()
                    .with_prompt("Search model ID/name (e.g. gpt-oss, qwen)")
                    .interact_text()?;
                if term.trim().is_empty() {
                    continue;
                }
                GroqModelQuery::Search(term)
            }
            Some(2) => {
                let mut authors: Vec<_> = models
                    .iter()
                    .filter(|m| m.supports_agent())
                    .map(|m| m.owned_by.clone())
                    .collect();
                authors.sort();
                authors.dedup();
                if authors.is_empty() {
                    ui::print_error("No verified coding models found in this catalog.");
                    continue;
                }
                let Some(index) = Select::new()
                    .with_prompt("Model author")
                    .items(&authors)
                    .max_length(MODEL_PAGE_SIZE)
                    .interact_opt()?
                else {
                    continue;
                };
                GroqModelQuery::Author(authors[index].clone())
            }
            Some(3) => GroqModelQuery::FullCatalog,
            _ => return Ok(None),
        };
        let matches = groq_models::filter_models(&models, &query);
        if matches.is_empty() {
            ui::print_error("No coding models matched. Try another search or refresh the catalog.");
            continue;
        }
        let labels: Vec<_> = matches.iter().map(|m| m.label()).collect();
        let default = matches.iter().position(|m| m.id == current).unwrap_or(0);
        let Some(index) = Select::new()
            .with_prompt(format!("Select model ({} found)", matches.len()))
            .items(&labels)
            .default(default)
            .max_length(MODEL_PAGE_SIZE)
            .interact_opt()?
        else {
            continue;
        };
        if !matches[index].supports_agent() {
            ui::print_error("This model is inactive or not verified for coding with local tools. Select a model from Recommended for coding.");
            continue;
        }
        return Ok(Some(matches[index].id.clone()));
    }
}

async fn handle_groq_jev_tiers(config: &mut Config) -> Result<()> {
    println!("{}", style("Fetching the latest Groq model catalog…").dim());
    let models =
        groq_models::fetch_models(config.groq_api_key.as_deref().unwrap_or(""), true).await?;
    println!("{}", style("Groq's catalog does not publish live price or quality scores. Recommendations use verified coding models currently available to your account.").dim());
    loop {
        let tiers = &config.groq_jev_models;
        let labels = [
            format!("Prep (planning/review): {}", tiers.prep),
            format!("Code (routine coding): {}", tiers.code),
            format!("Code-hard (difficult coding): {}", tiers.code_hard),
            format!("Quick (chat/explanations): {}", tiers.quick),
            "Back".into(),
        ];
        let Some(tier) = Select::new()
            .with_prompt("Groq Jev tier models")
            .items(&labels)
            .interact_opt()?
        else {
            return Ok(());
        };
        let (tier_name, current) = match tier {
            0 => ("Prep (planning/review)", tiers.prep.clone()),
            1 => ("Code (routine coding)", tiers.code.clone()),
            2 => ("Code-hard (difficult coding)", tiers.code_hard.clone()),
            3 => ("Quick (chat/explanations)", tiers.quick.clone()),
            _ => return Ok(()),
        };
        if let Some(model) =
            handle_groq_jev_model_picker(config, &models, tier_name, &current).await?
        {
            let mut updated = config.groq_jev_models.clone();
            match tier {
                0 => updated.prep = model,
                1 => updated.code = model,
                2 => updated.code_hard = model,
                _ => updated.quick = model,
            }
            config.set_groq_jev_models(updated)?;
            println!("{}", style("Saved Groq Jev tier models.").green());
        }
    }
}

async fn handle_groq_jev_model_picker(
    config: &Config,
    models: &[groq_models::GroqModel],
    tier_name: &str,
    current: &str,
) -> Result<Option<String>> {
    let recommendations = groq_models::jev_recommendations(models);
    let mut items: Vec<String> = recommendations
        .iter()
        .map(|recommendation| format!("{} — {}", recommendation.band, recommendation.model.label()))
        .collect();
    items.push("Browse all verified models from this catalog".to_string());
    items.push("Back".to_string());

    let Some(selection) = Select::new()
        .with_prompt(format!("Choose a Groq model for {tier_name}"))
        .items(&items)
        .default(
            recommendations
                .iter()
                .position(|recommendation| recommendation.model.id == current)
                .unwrap_or(0),
        )
        .max_length(MODEL_PAGE_SIZE)
        .interact_opt()?
    else {
        return Ok(None);
    };

    if let Some(recommendation) = recommendations.get(selection) {
        return Ok(Some(recommendation.model.id.clone()));
    }
    if selection == recommendations.len() {
        return handle_groq_model_picker(config, current).await;
    }
    Ok(None)
}

async fn handle_openrouter_menu(
    config: &mut Config,
    client: &mut Option<GroqClient>,
) -> Result<()> {
    loop {
        let items = vec![
            "Add or update OpenRouter API key".to_string(),
            "Select model".to_string(),
            "Switch to OpenRouter".to_string(),
            if config.jev_routing {
                "Turn Jev routing off".to_string()
            } else {
                "Turn Jev routing on".to_string()
            },
            "Choose Jev tier models".to_string(),
            "Refresh model catalog".to_string(),
            "Back".to_string(),
        ];
        let sel = Select::new()
            .with_prompt(format!(
                "OpenRouter (current: {}, jev {})",
                config.openrouter_model,
                if config.jev_routing { "on" } else { "off" }
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
                if config
                    .openrouter_api_key
                    .as_deref()
                    .is_none_or(|k| k.trim().is_empty())
                {
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
                if config
                    .openrouter_api_key
                    .as_deref()
                    .is_none_or(|key| key.trim().is_empty())
                {
                    ui::print_error("Add an OpenRouter API key first.");
                    continue;
                }
                let enabled = !config.jev_routing;
                config.set_jev_routing(enabled)?;
                println!(
                    "{}",
                    style(format!(
                        "Jev routing {} — VYBRID_JEV_ROUTING={} — settings:\n  {}",
                        if enabled { "on" } else { "off" },
                        if enabled { "on" } else { "off" },
                        saved_env_locations(config)
                    ))
                    .green()
                );
                if enabled {
                    println!(
                        "{}",
                        style(format!(
                            "Each OpenRouter turn asks Jev, then uses a tier model. {} stays the fallback.",
                            config.openrouter_model
                        ))
                        .dim()
                    );
                }
            }
            4 => {
                handle_jev_tier_models(config).await?;
                println!(
                    "{}",
                    style(format!(
                        "Saved Jev tier models — settings:\n  {}",
                        saved_env_locations(config)
                    ))
                    .green()
                );
            }
            5 => {
                clear_model_cache()?;
                println!("{}", style("OpenRouter model cache cleared.").green());
            }
            _ => break,
        }
    }
    Ok(())
}

async fn handle_jev_tier_models(config: &mut Config) -> Result<()> {
    let api_key = config
        .openrouter_api_key
        .as_deref()
        .context("OpenRouter API key missing")?;
    println!(
        "{}",
        style("Fetching the latest OpenRouter model catalog…").dim()
    );
    let models = fetch_models(api_key, &ModelQuery::JevRecommendations, true).await?;
    if models.is_empty() {
        anyhow::bail!("The latest OpenRouter catalog returned no tool-capable models.");
    }

    loop {
        let tiers = config.jev_tier_models();
        let labels = [
            format!("Prep (planning/review): {}", tiers.prep),
            format!("Code (routine coding): {}", tiers.code),
            format!("Code-hard (difficult coding): {}", tiers.code_hard),
            format!("Quick (chat/explanations): {}", tiers.quick),
            "Back".to_string(),
        ];
        let Some(tier) = Select::new()
            .with_prompt("OpenRouter Jev tier models")
            .items(&labels)
            .default(0)
            .interact_opt()?
        else {
            return Ok(());
        };
        let (tier_name, current) = match tier {
            0 => ("Prep (planning/review)", tiers.prep),
            1 => ("Code (routine coding)", tiers.code),
            2 => ("Code-hard (difficult coding)", tiers.code_hard),
            3 => ("Quick (chat/explanations)", tiers.quick),
            _ => return Ok(()),
        };
        let Some(model) = handle_openrouter_jev_model_picker(&models, tier_name, &current).await?
        else {
            continue;
        };
        match tier {
            0 => config.set_jev_model_prep(model)?,
            1 => config.set_jev_model_code(model)?,
            2 => config.set_jev_model_code_hard(model)?,
            _ => config.set_jev_model_quick(model)?,
        }
        println!("{}", style("Saved Jev tier model.").green());
    }
}

async fn handle_openrouter_jev_model_picker(
    models: &[OpenRouterModel],
    tier_name: &str,
    current: &str,
) -> Result<Option<String>> {
    loop {
        let recommendations = crate::client::openrouter::jev_recommendations(models);
        let mut items: Vec<String> = recommendations
            .iter()
            .map(|recommendation| {
                format!(
                    "{} — {}",
                    recommendation.band,
                    format_model_label(&recommendation.model, false)
                )
            })
            .collect();
        items.push("Search the refreshed catalog".to_string());
        items.push("Browse the refreshed catalog".to_string());
        items.push("Back".to_string());

        let Some(selection) = Select::new()
            .with_prompt(format!("Choose an OpenRouter model for {tier_name}"))
            .items(&items)
            .default(
                recommendations
                    .iter()
                    .position(|recommendation| recommendation.model.id == current)
                    .unwrap_or(0),
            )
            .max_length(MODEL_PAGE_SIZE)
            .interact_opt()?
        else {
            return Ok(None);
        };

        if let Some(recommendation) = recommendations.get(selection) {
            return Ok(Some(recommendation.model.id.clone()));
        }
        if selection == recommendations.len() {
            let term: String = Input::new()
                .with_prompt("Search refreshed catalog by model name or ID")
                .interact_text()?;
            let term = term.trim().to_ascii_lowercase();
            if term.is_empty() {
                continue;
            }
            let matches: Vec<_> = models
                .iter()
                .filter(|model| {
                    model.supports_tools
                        && (model.id.to_ascii_lowercase().contains(&term)
                            || model.name.to_ascii_lowercase().contains(&term)
                            || model.description.to_ascii_lowercase().contains(&term))
                })
                .collect();
            if matches.is_empty() {
                ui::print_error("No tool-capable models matched that search.");
                continue;
            }
            if let Some(model) =
                select_openrouter_catalog_model(&matches, current, "Search results")?
            {
                return Ok(Some(model));
            }
            continue;
        }
        if selection == recommendations.len() + 1 {
            let matches: Vec<_> = models.iter().filter(|model| model.supports_tools).collect();
            if let Some(model) =
                select_openrouter_catalog_model(&matches, current, "Current catalog")?
            {
                return Ok(Some(model));
            }
            continue;
        }
        return Ok(None);
    }
}

fn select_openrouter_catalog_model(
    models: &[&OpenRouterModel],
    current: &str,
    prompt: &str,
) -> Result<Option<String>> {
    let labels: Vec<_> = models
        .iter()
        .map(|model| format_model_label(model, false))
        .collect();
    let default = models
        .iter()
        .position(|model| model.id == current)
        .unwrap_or(0);
    let Some(index) = Select::new()
        .with_prompt(format!("{prompt} ({} models)", models.len()))
        .items(&labels)
        .default(default)
        .max_length(MODEL_PAGE_SIZE)
        .interact_opt()?
    else {
        return Ok(None);
    };
    Ok(models.get(index).map(|model| model.id.clone()))
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

async fn handle_lm_studio_menu(config: &mut Config, client: &mut Option<GroqClient>) -> Result<()> {
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
