//! Groq's authenticated model catalog. The list API does not advertise local
//! tool support, so agent compatibility is explicit and checked against live IDs.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::{Path, PathBuf};
use std::time::Duration;

const MODELS_ENDPOINT: &str = "https://api.groq.com/openai/v1/models";
const CACHE_TTL: i64 = 6 * 3600;
const CATALOG_TIMEOUT: Duration = Duration::from_secs(8);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelQuery {
    Recommended,
    Search(String),
    Author(String),
    FullCatalog,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroqModel {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub owned_by: String,
    #[serde(default = "active_default")]
    pub active: bool,
    pub context_window: Option<u32>,
}

#[derive(Debug, Clone)]
pub struct JevRecommendation {
    pub band: &'static str,
    pub model: GroqModel,
}

fn active_default() -> bool {
    true
}

impl GroqModel {
    pub fn supports_agent(&self) -> bool {
        self.active && supports_local_tools(&self.id)
    }

    pub fn label(&self) -> String {
        let status = if !self.active {
            "[inactive] "
        } else if !self.supports_agent() {
            "[not verified for coding] "
        } else {
            ""
        };
        let context = super::openrouter::format_context_length(self.context_window);
        let name = self
            .name
            .as_deref()
            .filter(|name| *name != self.id)
            .map(|name| format!(" — {name}"))
            .unwrap_or_default();
        format!("{status}{}{name} · {} · {context}", self.id, self.owned_by)
    }
}

/// Known local function-calling chat models. Older IDs are allowed only when
/// still returned by Groq (some enterprise accounts retain retired models).
/// Capability reference: https://console.groq.com/docs/tool-use/overview
/// Never infer support from a name prefix: safeguard/guard/audio models can
/// share an author and family with general chat models.
pub fn supports_local_tools(model: &str) -> bool {
    matches!(
        model,
        "openai/gpt-oss-120b"
            | "openai/gpt-oss-20b"
            | "qwen/qwen3.8-27b"
            | "qwen/qwen3.6-27b"
            | "qwen/qwen3-32b"
            | "minimaxai/minimax-m2.7"
            | "meta-models/muse-spark-open"
            | "llama-3.3-70b-versatile"
            | "llama-3.1-8b-instant"
            | "meta-llama/llama-4-scout-17b-16e-instruct"
            | "moonshotai/kimi-k2-instruct-0905"
    )
}

pub fn is_available(models: &[GroqModel], id: &str) -> bool {
    models.iter().any(|m| m.id == id && m.supports_agent())
}

/// Groq's live models endpoint publishes availability and context size, but
/// not price or quality scores. Keep these recommendation profiles explicit,
/// and intersect them with the freshly fetched catalog and verified tool IDs.
pub fn jev_recommendations(models: &[GroqModel]) -> Vec<JevRecommendation> {
    let bands: [(&str, &[&str]); 3] = [
        (
            "Premium · strongest available model",
            &[
                "meta-models/muse-spark-open",
                "minimaxai/minimax-m2.7",
                "qwen/qwen3.8-27b",
            ],
        ),
        (
            "Mid-cost · strong capability and cost balance",
            &[
                "openai/gpt-oss-120b",
                "moonshotai/kimi-k2-instruct-0905",
                "qwen/qwen3.6-27b",
                "qwen/qwen3-32b",
                "llama-3.3-70b-versatile",
            ],
        ),
        (
            "Budget · best performance for the cost",
            &["openai/gpt-oss-20b", "llama-3.1-8b-instant"],
        ),
    ];
    let mut selected = Vec::new();
    for (band, preferred_ids) in bands {
        let model = preferred_ids
            .iter()
            .find_map(|id| {
                models
                    .iter()
                    .find(|model| model.id == *id && model.supports_agent())
            })
            .filter(|model| {
                !selected
                    .iter()
                    .any(|item: &JevRecommendation| item.model.id == model.id)
            });
        if let Some(model) = model {
            selected.push(JevRecommendation {
                band,
                model: model.clone(),
            });
        }
    }
    selected
}

pub fn filter_models(models: &[GroqModel], query: &ModelQuery) -> Vec<GroqModel> {
    let mut filtered: Vec<_> = models
        .iter()
        .filter(|model| {
            if matches!(query, ModelQuery::FullCatalog) {
                return true;
            }
            if !model.supports_agent() {
                return false;
            }
            match query {
                ModelQuery::Search(term) => {
                    let term = term.trim().to_ascii_lowercase();
                    model.id.to_ascii_lowercase().contains(&term)
                        || model
                            .name
                            .as_deref()
                            .unwrap_or("")
                            .to_ascii_lowercase()
                            .contains(&term)
                        || model.owned_by.to_ascii_lowercase().contains(&term)
                }
                ModelQuery::Author(author) => model.owned_by.eq_ignore_ascii_case(author.trim()),
                _ => true,
            }
        })
        .cloned()
        .collect();
    filtered.sort_by_key(|m| {
        let rank = if matches!(query, ModelQuery::Recommended) {
            match m.id.as_str() {
                "openai/gpt-oss-120b" => 0,
                "openai/gpt-oss-20b" => 1,
                _ => 2,
            }
        } else {
            0
        };
        (rank, m.id.clone())
    });
    filtered
}

#[derive(Debug, Serialize, Deserialize)]
struct ModelsResponse {
    data: Vec<GroqModel>,
}

#[derive(Debug, Serialize, Deserialize)]
struct ModelsCache {
    fetched_at: DateTime<Utc>,
    models: Vec<GroqModel>,
}

/// Scope cached catalogs to the credential without storing the credential.
fn cache_path(api_key: &str) -> Option<PathBuf> {
    let mut hash = DefaultHasher::new();
    api_key.trim().hash(&mut hash);
    Some(
        dirs::home_dir()?
            .join(".vybrid/cache")
            .join(format!("groq_{:016x}.json", hash.finish())),
    )
}

fn read_cache(path: &Path) -> Option<Vec<GroqModel>> {
    let cached: ModelsCache = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    let age = Utc::now()
        .signed_duration_since(cached.fetched_at)
        .num_seconds();
    (0..CACHE_TTL).contains(&age).then_some(cached.models)
}

fn write_cache(path: &Path, models: &[GroqModel]) -> Result<()> {
    let parent = path
        .parent()
        .context("Model cache has no parent directory")?;
    std::fs::create_dir_all(parent)?;
    let temporary = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
    let cached = ModelsCache {
        fetched_at: Utc::now(),
        models: models.to_vec(),
    };
    let result = (|| -> Result<()> {
        std::fs::write(&temporary, serde_json::to_vec(&cached)?)?;
        std::fs::rename(&temporary, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(temporary);
    }
    result
}

async fn fetch_at(
    api_key: &str,
    endpoint: &str,
    cache: Option<&Path>,
    force_refresh: bool,
) -> Result<Vec<GroqModel>> {
    let api_key = api_key.trim();
    if api_key.is_empty() {
        anyhow::bail!("Add a Groq API key first.");
    }
    if !force_refresh {
        if let Some(models) = cache.and_then(read_cache) {
            return Ok(models);
        }
    }
    let response = reqwest::Client::builder()
        .timeout(CATALOG_TIMEOUT)
        .build()?
        .get(endpoint)
        .bearer_auth(api_key)
        .send()
        .await
        .context("Failed to fetch Groq models")?;
    let status = response.status();
    if !status.is_success() {
        // Do not echo response bodies or request headers containing credentials.
        anyhow::bail!(
            "Groq models API error ({status}). Check your API key and model permissions."
        );
    }
    let models = response
        .json::<ModelsResponse>()
        .await
        .context("Invalid Groq model catalog")?
        .data;
    if let Some(path) = cache {
        // A read-only home directory or damaged cache must not break model selection.
        let _ = write_cache(path, &models);
    }
    Ok(models)
}

pub async fn fetch_models(api_key: &str, force_refresh: bool) -> Result<Vec<GroqModel>> {
    fetch_at(
        api_key,
        MODELS_ENDPOINT,
        cache_path(api_key).as_deref(),
        force_refresh,
    )
    .await
}

/// Select a distinct, verified fallback. If discovery failed or no alternative
/// is available, stay on the primary and let its normal retry/wait policy run.
pub fn fallback_model(
    models: Option<&[GroqModel]>,
    configured: &str,
    pinned: &str,
    primary: &str,
) -> String {
    if let Some(models) = models {
        for id in [
            configured,
            pinned,
            "openai/gpt-oss-20b",
            "openai/gpt-oss-120b",
        ] {
            if id != primary && is_available(models, id) {
                return id.to_string();
            }
        }
    }
    primary.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn fixture() -> Vec<GroqModel> {
        serde_json::from_value::<ModelsResponse>(serde_json::json!({"data": [
            {"id":"openai/gpt-oss-120b", "owned_by":"OpenAI", "context_window":131072},
            {"id":"openai/gpt-oss-20b", "owned_by":"OpenAI", "name":"GPT OSS Small"},
            {"id":"qwen/qwen3.8-27b", "owned_by":"Alibaba"},
            {"id":"llama-3.1-8b-instant", "owned_by":"Meta", "active":false},
            {"id":"whisper-large-v3", "owned_by":"OpenAI"},
            {"id":"openai/gpt-oss-safeguard-20b", "owned_by":"OpenAI"},
            {"id":"groq/compound", "owned_by":"Groq"},
            {"id":"unknown/new-model", "owned_by":"New Author"}
        ]}))
        .unwrap()
        .data
    }

    struct TestDirectory(PathBuf);
    impl TestDirectory {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!("vybrid-groq-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    async fn serve(status: &str, body: &str) -> (String, tokio::task::JoinHandle<String>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/openai/v1/models", listener.local_addr().unwrap());
        let response = format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
        let server = tokio::spawn(async move {
            tokio::time::timeout(Duration::from_secs(5), async move {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                let mut buffer = [0; 1024];
                while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                    let count = socket.read(&mut buffer).await.unwrap();
                    assert!(count > 0);
                    request.extend_from_slice(&buffer[..count]);
                }
                socket.write_all(response.as_bytes()).await.unwrap();
                String::from_utf8(request).unwrap()
            })
            .await
            .unwrap()
        });
        (endpoint, server)
    }

    #[test]
    fn coding_catalog_excludes_non_chat_inactive_and_unknown_models() {
        let models = fixture();
        let coding = filter_models(&models, &ModelQuery::Recommended);
        assert_eq!(
            coding.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
            [
                "openai/gpt-oss-120b",
                "openai/gpt-oss-20b",
                "qwen/qwen3.8-27b"
            ]
        );
        assert_eq!(filter_models(&models, &ModelQuery::FullCatalog).len(), 8);
        assert!(models[3].label().contains("inactive"));
        assert!(models[4].label().contains("not verified"));
        assert!(models[0].label().contains("131k ctx"));
    }

    #[test]
    fn searches_ids_names_and_authors_case_insensitively() {
        let models = fixture();
        assert_eq!(
            filter_models(&models, &ModelQuery::Search(" SMALL ".into()))[0].id,
            "openai/gpt-oss-20b"
        );
        assert_eq!(
            filter_models(&models, &ModelQuery::Search("QWEN".into()))[0].id,
            "qwen/qwen3.8-27b"
        );
        assert_eq!(
            filter_models(&models, &ModelQuery::Author(" openai ".into())).len(),
            2
        );
        assert!(filter_models(&models, &ModelQuery::Search("whisper".into())).is_empty());
        assert!(filter_models(&models, &ModelQuery::Search("missing".into())).is_empty());
    }

    #[test]
    fn fallback_avoids_retired_incompatible_and_primary_ids() {
        let models = fixture();
        assert_eq!(
            fallback_model(
                Some(&models),
                "qwen/qwen3-32b",
                "openai/gpt-oss-120b",
                "openai/gpt-oss-120b"
            ),
            "openai/gpt-oss-20b"
        );
        assert_eq!(
            fallback_model(
                Some(&models),
                "groq/compound",
                "openai/gpt-oss-120b",
                "openai/gpt-oss-20b"
            ),
            "openai/gpt-oss-120b"
        );
        assert_eq!(
            fallback_model(None, "openai/gpt-oss-20b", "pinned", "primary"),
            "primary"
        );
        assert_eq!(
            fallback_model(Some(&[]), "anything", "pinned", "primary"),
            "primary"
        );
    }

    #[test]
    fn cache_is_scoped_to_credentials_and_never_contains_the_key() {
        let first = cache_path("groq-test-secret").unwrap();
        assert_ne!(first, cache_path("another-key").unwrap());
        assert_eq!(first, cache_path(" groq-test-secret ").unwrap());
        assert!(!first.to_string_lossy().contains("groq-test-secret"));
    }

    #[test]
    fn invalid_expired_and_future_caches_are_ignored() {
        let directory = TestDirectory::new();
        let path = directory.0.join("models.json");
        std::fs::write(&path, "broken JSON").unwrap();
        assert!(read_cache(&path).is_none());
        for offset in [-CACHE_TTL - 1, 100] {
            let cache = ModelsCache {
                fetched_at: Utc::now() + chrono::Duration::seconds(offset),
                models: fixture(),
            };
            std::fs::write(&path, serde_json::to_vec(&cache).unwrap()).unwrap();
            assert!(read_cache(&path).is_none());
        }
        write_cache(&path, &fixture()).unwrap();
        assert_eq!(read_cache(&path).unwrap().len(), 8);
    }

    #[tokio::test]
    async fn authenticated_catalog_fetch_cache_and_force_refresh() {
        let directory = TestDirectory::new();
        let path = directory.0.join("models.json");
        let body = serde_json::json!({"data":fixture()}).to_string();
        let (endpoint, server) = serve("200 OK", &body).await;
        let models = fetch_at(" groq-test-key ", &endpoint, Some(&path), false)
            .await
            .unwrap();
        assert_eq!(models.len(), 8);
        let request = server.await.unwrap().to_ascii_lowercase();
        assert!(request.starts_with("get /openai/v1/models "));
        assert!(request.contains("authorization: bearer groq-test-key\r\n"));
        assert!(!std::fs::read_to_string(&path)
            .unwrap()
            .contains("groq-test-key"));
        // A cache hit must not touch the network.
        assert_eq!(
            fetch_at("groq-test-key", "http://127.0.0.1:1", Some(&path), false)
                .await
                .unwrap()
                .len(),
            8
        );
        let (endpoint, server) = serve("200 OK", r#"{"data":[]}"#).await;
        assert!(fetch_at("groq-test-key", &endpoint, Some(&path), true)
            .await
            .unwrap()
            .is_empty());
        server.await.unwrap();
        assert!(read_cache(&path).unwrap().is_empty());
    }

    #[tokio::test]
    async fn catalog_errors_do_not_expose_body_or_use_cache_without_a_key() {
        for status in [
            "401 Unauthorized",
            "429 Too Many Requests",
            "500 Internal Server Error",
        ] {
            let (endpoint, server) = serve(status, "secret-from-response-body").await;
            let error = fetch_at("groq-test-key", &endpoint, None, true)
                .await
                .unwrap_err()
                .to_string();
            assert!(error.contains(&status[..3]));
            assert!(!error.contains("secret-from-response-body"));
            server.await.unwrap();
        }
        let directory = TestDirectory::new();
        let path = directory.0.join("models.json");
        write_cache(&path, &fixture()).unwrap();
        assert!(fetch_at(" ", "http://127.0.0.1:1", Some(&path), false)
            .await
            .unwrap_err()
            .to_string()
            .contains("API key"));
    }

    #[tokio::test]
    async fn corrupt_or_unwritable_cache_does_not_break_discovery() {
        let directory = TestDirectory::new();
        let path = directory.0.join("models.json");
        std::fs::write(&path, "invalid JSON").unwrap();
        let (endpoint, server) = serve("200 OK", r#"{"data":[]}"#).await;
        assert!(fetch_at("groq-test-key", &endpoint, Some(&path), false)
            .await
            .unwrap()
            .is_empty());
        server.await.unwrap();
        let (endpoint, server) = serve("200 OK", r#"{"data":[]}"#).await;
        assert!(fetch_at(
            "groq-test-key",
            &endpoint,
            Some(&path.join("cannot-write.json")),
            false
        )
        .await
        .unwrap()
        .is_empty());
        server.await.unwrap();
    }
}
