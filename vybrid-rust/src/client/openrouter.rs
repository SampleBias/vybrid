#![allow(dead_code)]

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::Duration;

const MODELS_ENDPOINT: &str = "https://openrouter.ai/api/v1/models";
const CACHE_TTL: Duration = Duration::from_secs(6 * 3600);

/// Popular model authors for the provider-first browse path.
pub const POPULAR_PROVIDERS: &[(&str, &str)] = &[
    ("anthropic", "Anthropic"),
    ("openai", "OpenAI"),
    ("google", "Google"),
    ("meta-llama", "Meta Llama"),
    ("deepseek", "DeepSeek"),
    ("mistralai", "Mistral"),
    ("qwen", "Qwen"),
    ("cohere", "Cohere"),
    ("x-ai", "xAI"),
];

/// Which slice of the OpenRouter catalog to fetch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelQuery {
    /// Programming category + most-popular; tool-capable models filtered client-side
    /// (OpenRouter rejects `category` + `supported_parameters` together).
    Recommended,
    /// Free-text search (`q` param) with tools filter.
    Search(String),
    /// Models from one author/provider slug.
    Provider(String),
    /// Complete catalog (advanced).
    FullCatalog,
}

impl ModelQuery {
    fn cache_key(&self) -> String {
        match self {
            Self::Recommended => "recommended".to_string(),
            Self::Search(term) => format!("search:{}", term.trim().to_ascii_lowercase()),
            Self::Provider(author) => format!("provider:{}", author.trim().to_ascii_lowercase()),
            Self::FullCatalog => "full".to_string(),
        }
    }

    fn query_params(&self) -> Vec<(&str, String)> {
        match self {
            Self::Recommended => vec![
                ("category", "programming".to_string()),
                ("sort", "most-popular".to_string()),
            ],
            Self::Search(term) => vec![
                ("q", term.trim().to_string()),
                ("supported_parameters", "tools".to_string()),
            ],
            Self::Provider(author) => vec![
                ("model_authors", author.trim().to_string()),
                ("supported_parameters", "tools".to_string()),
                ("sort", "most-popular".to_string()),
            ],
            Self::FullCatalog => vec![],
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenRouterModel {
    pub id: String,
    pub name: String,
    pub description: String,
    pub context_length: Option<u32>,
    pub supports_tools: bool,
    pub prompt_price: Option<String>,
    pub completion_price: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct ModelsCacheFile {
    query_key: String,
    fetched_at: DateTime<Utc>,
    models: Vec<OpenRouterModel>,
}

#[derive(Debug, Deserialize)]
struct ModelsListResponse {
    data: Vec<ApiModel>,
}

#[derive(Debug, Deserialize)]
struct ApiModel {
    id: String,
    name: String,
    #[serde(default)]
    description: String,
    context_length: Option<u32>,
    #[serde(default)]
    supported_parameters: Vec<String>,
    pricing: Option<ApiPricing>,
}

#[derive(Debug, Deserialize)]
struct ApiPricing {
    prompt: Option<String>,
    completion: Option<String>,
}

fn cache_dir() -> Result<PathBuf> {
    let home = dirs::home_dir().context("Could not find home directory")?;
    let dir = home.join(".vybrid").join("cache");
    std::fs::create_dir_all(&dir).context("Failed to create ~/.vybrid/cache")?;
    Ok(dir)
}

fn cache_path(query_key: &str) -> Result<PathBuf> {
    let safe: String = query_key
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect();
    Ok(cache_dir()?.join(format!("openrouter_{safe}.json")))
}

fn read_cache(query: &ModelQuery) -> Result<Option<Vec<OpenRouterModel>>> {
    let path = cache_path(&query.cache_key())?;
    if !path.exists() {
        return Ok(None);
    }
    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("Failed to read {}", path.display()))?;
    let cached: ModelsCacheFile = serde_json::from_str(&content)
        .with_context(|| format!("Failed to parse {}", path.display()))?;
    if cached.query_key != query.cache_key() {
        return Ok(None);
    }
    let age = Utc::now().signed_duration_since(cached.fetched_at);
    if age.to_std().unwrap_or(CACHE_TTL) >= CACHE_TTL {
        return Ok(None);
    }
    Ok(Some(cached.models))
}

fn write_cache(query: &ModelQuery, models: &[OpenRouterModel]) -> Result<()> {
    let path = cache_path(&query.cache_key())?;
    let file = ModelsCacheFile {
        query_key: query.cache_key(),
        fetched_at: Utc::now(),
        models: models.to_vec(),
    };
    let json = serde_json::to_string_pretty(&file).context("Failed to serialize model cache")?;
    std::fs::write(&path, json).with_context(|| format!("Failed to write {}", path.display()))?;
    Ok(())
}

fn parse_models(data: Vec<ApiModel>) -> Vec<OpenRouterModel> {
    data.into_iter()
        .map(|m| {
            let supports_tools = m.supported_parameters.iter().any(|p| p == "tools");
            OpenRouterModel {
                id: m.id,
                name: m.name,
                description: m.description,
                context_length: m.context_length,
                supports_tools,
                prompt_price: m.pricing.as_ref().and_then(|p| p.prompt.clone()),
                completion_price: m.pricing.as_ref().and_then(|p| p.completion.clone()),
            }
        })
        .collect()
}

/// Apply query-specific filters that cannot be expressed as combined API params.
fn post_filter(models: Vec<OpenRouterModel>, query: &ModelQuery) -> Vec<OpenRouterModel> {
    match query {
        ModelQuery::Recommended => models
            .into_iter()
            .filter(|m| m.supports_tools)
            .collect(),
        _ => models,
    }
}

async fn fetch_from_api(api_key: &str, query: &ModelQuery) -> Result<Vec<OpenRouterModel>> {
    let client = Client::builder()
        .timeout(Duration::from_secs(60))
        .build()
        .context("Failed to create HTTP client")?;

    let mut request = client
        .get(MODELS_ENDPOINT)
        .header("Authorization", format!("Bearer {api_key}"));

    for (key, value) in query.query_params() {
        if !value.is_empty() {
            request = request.query(&[(key, value)]);
        }
    }

    let response = request
        .send()
        .await
        .context("Failed to fetch OpenRouter models")?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response
            .text()
            .await
            .unwrap_or_else(|_| "Unknown error".to_string());
        anyhow::bail!("OpenRouter models API error ({status}): {body}");
    }

    let parsed: ModelsListResponse = response
        .json()
        .await
        .context("Failed to parse OpenRouter models response")?;

    Ok(post_filter(parse_models(parsed.data), query))
}

/// Fetch models for the given query, using a local cache unless `force_refresh`.
pub async fn fetch_models(
    api_key: &str,
    query: &ModelQuery,
    force_refresh: bool,
) -> Result<Vec<OpenRouterModel>> {
    if !force_refresh {
        if let Some(cached) = read_cache(query)? {
            return Ok(cached);
        }
    }

    let models = fetch_from_api(api_key, query).await?;
    write_cache(query, &models)?;
    Ok(models)
}

pub fn format_context_length(ctx: Option<u32>) -> String {
    match ctx {
        Some(n) if n >= 1_000_000 => format!("{:.1}M ctx", n as f64 / 1_000_000.0),
        Some(n) if n >= 1_000 => format!("{}k ctx", n / 1_000),
        Some(n) => format!("{n} ctx"),
        None => "? ctx".to_string(),
    }
}

fn format_price_per_million(price: &str) -> String {
    price
        .parse::<f64>()
        .map(|n| format!("${:.2}", n * 1_000_000.0))
        .unwrap_or_else(|_| "?".to_string())
}

/// Compact label for dialoguer menus.
pub fn format_model_label(model: &OpenRouterModel, show_no_tools: bool) -> String {
    let prefix = if show_no_tools && !model.supports_tools {
        "[no tools] "
    } else {
        ""
    };
    let ctx = format_context_length(model.context_length);
    let pricing = match (&model.prompt_price, &model.completion_price) {
        (Some(p), Some(c)) => format!(
            "{} / {} per M",
            format_price_per_million(p),
            format_price_per_million(c)
        ),
        _ => String::new(),
    };
    if pricing.is_empty() {
        format!("{prefix}{} — {} · {}", model.id, model.name, ctx)
    } else {
        format!(
            "{prefix}{} — {} · {} · {}",
            model.id, model.name, ctx, pricing
        )
    }
}

/// Invalidate all cached OpenRouter model lists.
pub fn clear_model_cache() -> Result<()> {
    let dir = cache_dir()?;
    if !dir.exists() {
        return Ok(());
    }
    for entry in std::fs::read_dir(&dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with("openrouter_") && name.ends_with(".json") {
            let _ = std::fs::remove_file(entry.path());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_models_detects_tools_support() {
        let json = r#"{
            "data": [
                {
                    "id": "anthropic/claude-sonnet-4",
                    "name": "Claude Sonnet 4",
                    "description": "Smart model",
                    "context_length": 200000,
                    "supported_parameters": ["temperature", "tools"],
                    "pricing": { "prompt": "0.000003", "completion": "0.000015" }
                },
                {
                    "id": "provider/no-tools",
                    "name": "No Tools",
                    "description": "",
                    "context_length": 8192,
                    "supported_parameters": ["temperature"],
                    "pricing": { "prompt": "0", "completion": "0" }
                }
            ]
        }"#;
        let resp: ModelsListResponse = serde_json::from_str(json).unwrap();
        let models = parse_models(resp.data);
        assert_eq!(models.len(), 2);
        assert!(models[0].supports_tools);
        assert!(!models[1].supports_tools);
    }

    #[test]
    fn model_query_cache_keys_differ() {
        assert_ne!(
            ModelQuery::Recommended.cache_key(),
            ModelQuery::Search("claude".into()).cache_key()
        );
        assert_eq!(
            ModelQuery::Provider("anthropic".into()).cache_key(),
            "provider:anthropic"
        );
    }

    #[test]
    fn format_model_label_marks_no_tools_in_full_catalog() {
        let model = OpenRouterModel {
            id: "x/y".into(),
            name: "Y".into(),
            description: String::new(),
            context_length: Some(128_000),
            supports_tools: false,
            prompt_price: Some("0.000001".into()),
            completion_price: Some("0.000002".into()),
        };
        let label = format_model_label(&model, true);
        assert!(label.starts_with("[no tools]"));
    }

    #[test]
    fn recommended_query_omits_supported_parameters_with_category() {
        let params: std::collections::HashMap<_, _> = ModelQuery::Recommended
            .query_params()
            .into_iter()
            .collect();
        assert_eq!(params.get("category").map(String::as_str), Some("programming"));
        assert!(!params.contains_key("supported_parameters"));
    }

    #[test]
    fn post_filter_recommended_keeps_only_tool_capable() {
        let models = vec![
            OpenRouterModel {
                id: "a/1".into(),
                name: "One".into(),
                description: String::new(),
                context_length: None,
                supports_tools: true,
                prompt_price: None,
                completion_price: None,
            },
            OpenRouterModel {
                id: "b/2".into(),
                name: "Two".into(),
                description: String::new(),
                context_length: None,
                supports_tools: false,
                prompt_price: None,
                completion_price: None,
            },
        ];
        let filtered = post_filter(models, &ModelQuery::Recommended);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].id, "a/1");
    }

    #[test]
    fn models_endpoint_uses_openrouter() {
        assert!(MODELS_ENDPOINT.contains("openrouter.ai"));
    }
}
