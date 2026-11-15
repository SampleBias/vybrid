//! Jev (TypeSafe System One) routing over the OpenRouter Decisions API.
//!
//! Jev returns typed probabilities. This module asks four questions about the
//! user task, then local code maps those answers onto a model tier.

use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::time::Duration;

use super::groq_models::{self, GroqModel};
use super::openrouter::{fetch_models, ModelQuery, OpenRouterModel};

const DECISIONS_URL: &str = "https://openrouter.ai/api/alpha/decisions";
const JEV_MODEL: &str = "~typesafe/jev-latest";
const DECISION_TIMEOUT: Duration = Duration::from_secs(8);
const TASK_MAX_CHARS: usize = 6_000;
const PRIOR_TASK_MAX_CHARS: usize = 2_000;
/// Latest task at or below this length is treated as a follow-up.
const FOLLOW_UP_MAX_CHARS: usize = 80;

/// Below this `task_kind` confidence, keep the active provider's pinned model.
pub const CONFIDENCE_FLOOR: f64 = 0.45;
/// Score index 1 is routine and 2 is involved. At or past the midpoint, coding
/// work uses the harder coding model and chat/explain uses the prep model.
pub const COMPLEXITY_INVOLVED: f64 = 1.5;
pub const DEEP_PREP_HIGH: f64 = 0.65;
pub const CONSEQUENCE_HIGH: f64 = 0.7;

#[derive(Debug, Clone)]
pub struct TierModels {
    pub prep: String,
    pub code: String,
    pub code_hard: String,
    pub quick: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Judgment {
    pub task_kind: String,
    pub task_kind_confidence: f64,
    pub complexity: f64,
    pub deep_preparation: f64,
    pub consequence: f64,
    /// USD cost from `usage.cost`. Routing does not depend on it.
    #[allow(dead_code)]
    pub cost_usd: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteChoice {
    pub label: &'static str,
    pub model: String,
    pub note: Option<&'static str>,
}

impl RouteChoice {
    pub fn is_fallback(&self) -> bool {
        self.label == "pinned"
    }

    pub fn status_line(&self) -> String {
        match self.note {
            Some(note) => format!("jev → {} {} ({note})", self.label, self.model),
            None => format!("jev → {} {}", self.label, self.model),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CatalogFit {
    Usable,
    Missing,
    NoTools,
    Unchecked,
}

pub fn brief_error(err: &anyhow::Error) -> String {
    let text = err.to_string().replace('\n', " ");
    let mut out = String::new();
    for (i, ch) in text.chars().enumerate() {
        if i >= 160 {
            out.push('…');
            break;
        }
        out.push(ch);
    }
    out
}

/// Decision state. A short latest task also includes the previous user task.
pub fn task_state(latest: &str, previous: Option<&str>) -> Value {
    let task = truncate_chars(latest.trim(), TASK_MAX_CHARS);
    let mut state = json!({ "task": task });
    let prior = previous
        .map(str::trim)
        .filter(|text| !text.is_empty() && task.chars().count() <= FOLLOW_UP_MAX_CHARS);
    if let Some(prior) = prior {
        state["prior_task"] = json!(truncate_chars(prior, PRIOR_TASK_MAX_CHARS));
    }
    state
}

pub fn select_tier(judgment: &Judgment, tiers: &TierModels, pinned: &str) -> RouteChoice {
    if judgment.task_kind_confidence < CONFIDENCE_FLOOR {
        return fallback_choice(pinned, Some("low confidence"));
    }
    let hard_code = judgment.complexity >= COMPLEXITY_INVOLVED
        || judgment.deep_preparation >= DEEP_PREP_HIGH
        || judgment.consequence >= CONSEQUENCE_HIGH;
    let escalate_quick =
        judgment.complexity >= COMPLEXITY_INVOLVED || judgment.deep_preparation >= DEEP_PREP_HIGH;

    match judgment.task_kind.as_str() {
        "plan" | "review" | "research" => choice("prep", &tiers.prep),
        "implement" | "debug" | "refactor" => {
            if hard_code {
                choice("code-hard", &tiers.code_hard)
            } else {
                choice("code", &tiers.code)
            }
        }
        "explain" | "chat" => {
            if escalate_quick {
                choice("prep", &tiers.prep)
            } else {
                choice("quick", &tiers.quick)
            }
        }
        _ => fallback_choice(pinned, Some("unrecognized task")),
    }
}

pub fn catalog_fit(models: &[OpenRouterModel], model_id: &str) -> CatalogFit {
    if model_id.starts_with('~') {
        return CatalogFit::Usable;
    }
    match models.iter().find(|model| model.id == model_id) {
        Some(model) if model.supports_tools => CatalogFit::Usable,
        Some(_) => CatalogFit::NoTools,
        None => CatalogFit::Missing,
    }
}

pub fn apply_catalog(choice: RouteChoice, fit: CatalogFit, pinned_model: &str) -> RouteChoice {
    if choice.is_fallback() {
        return choice;
    }
    match fit {
        CatalogFit::Usable | CatalogFit::Unchecked => choice,
        CatalogFit::Missing => fallback_choice(pinned_model, Some("model not in catalog")),
        CatalogFit::NoTools => fallback_choice(pinned_model, Some("model does not support tools")),
    }
}

/// Groq has no OpenRouter aliases or tool-capability flags. Only select a tier
/// that is present, active, and known to support the agent's local tools.
pub fn apply_groq_catalog(
    choice: RouteChoice,
    models: Option<&[GroqModel]>,
    pinned: &str,
) -> RouteChoice {
    if choice.is_fallback() {
        return choice;
    }
    let Some(models) = models else {
        return fallback_choice(pinned, Some("Groq catalog unavailable"));
    };
    let fit = match models.iter().find(|m| m.id == choice.model && m.active) {
        Some(model) if groq_models::supports_local_tools(&model.id) => CatalogFit::Usable,
        Some(_) => CatalogFit::NoTools,
        None => CatalogFit::Missing,
    };
    apply_catalog(choice, fit, pinned)
}

pub fn parse_judgment(body: &Value) -> Result<Judgment> {
    let answers = body
        .get("answers")
        .context("Jev response missing answers")?;
    let kind = answers
        .get("task_kind")
        .context("Jev response missing task_kind")?;
    let task_kind = kind
        .get("choice")
        .and_then(Value::as_str)
        .context("Jev response missing task_kind choice")?
        .trim()
        .to_ascii_lowercase();
    if task_kind.is_empty() {
        anyhow::bail!("Jev response had an empty task_kind");
    }
    let task_kind_confidence = kind.get("confidence").and_then(json_f64).unwrap_or(0.0);
    let complexity = answers
        .get("complexity")
        .and_then(|v| v.get("score"))
        .and_then(json_f64)
        .context("Jev response missing complexity score")?;
    let deep_preparation = answers
        .get("deep_preparation")
        .and_then(|v| v.get("noul"))
        .and_then(json_f64)
        .context("Jev response missing deep_preparation")?;
    let consequence = answers
        .get("consequence")
        .and_then(|v| v.get("noul"))
        .and_then(json_f64)
        .context("Jev response missing consequence")?;
    let cost_usd = body.get("usage").and_then(|usage| {
        usage
            .get("cost")
            .and_then(json_f64)
            .or_else(|| usage.get("cost_usd").and_then(json_f64))
    });
    Ok(Judgment {
        task_kind,
        task_kind_confidence,
        complexity,
        deep_preparation,
        consequence,
        cost_usd,
    })
}

/// Classify `latest_task` and choose a model. Catalog misses fall back to `pinned`.
/// A catalog fetch failure leaves the chosen model in place.
pub async fn route_turn(
    api_key: &str,
    latest_task: &str,
    previous_task: Option<&str>,
    tiers: &TierModels,
    pinned: &str,
) -> Result<RouteChoice> {
    let api_key = api_key.trim();
    let choice = classify_turn(api_key, latest_task, previous_task, tiers, pinned).await?;
    if choice.is_fallback() {
        return Ok(choice);
    }
    let fit = match fetch_models(api_key, &ModelQuery::FullCatalog, false).await {
        Ok(models) => catalog_fit(&models, &choice.model),
        Err(_) => CatalogFit::Unchecked,
    };
    Ok(apply_catalog(choice, fit, pinned))
}

/// Provider-independent classification. The caller validates the selected ID
/// against the chat provider's catalog; this request always uses OpenRouter.
pub async fn classify_turn(
    api_key: &str,
    latest_task: &str,
    previous_task: Option<&str>,
    tiers: &TierModels,
    pinned: &str,
) -> Result<RouteChoice> {
    let api_key = api_key.trim();
    if api_key.is_empty() {
        anyhow::bail!("OpenRouter API key is missing");
    }
    let judgment = judge(api_key, &task_state(latest_task, previous_task)).await?;
    Ok(select_tier(&judgment, tiers, pinned))
}

async fn judge(api_key: &str, state: &Value) -> Result<Judgment> {
    judge_at(api_key, state, DECISIONS_URL).await
}

async fn judge_at(api_key: &str, state: &Value, endpoint: &str) -> Result<Judgment> {
    let client = reqwest::Client::builder()
        .timeout(DECISION_TIMEOUT)
        .build()
        .context("Failed to create Jev HTTP client")?;
    let body = json!({
        "model": JEV_MODEL,
        "state": state,
        "questions": questions(),
    });
    let response = client
        .post(endpoint)
        .header("Authorization", format!("Bearer {api_key}"))
        .header("Content-Type", "application/json")
        .header("HTTP-Referer", "https://github.com/SampleBias/vybrid")
        .header("X-OpenRouter-Title", "Vybrid")
        .json(&body)
        .send()
        .await
        .context("Jev decision request failed")?;
    let status = response.status();
    let text = response
        .text()
        .await
        .context("Failed to read Jev decision response")?;
    if !status.is_success() {
        anyhow::bail!(
            "Jev decision API error ({status}): {}",
            truncate_chars(&text, 500)
        );
    }
    let parsed: Value =
        serde_json::from_str(&text).context("Jev decision response was not JSON")?;
    parse_judgment(&parsed)
}

fn questions() -> Value {
    json!({
        "task_kind": {
            "type": "choice",
            "instructions": "What kind of work is state.task for a coding agent? If state.prior_task is present, state.task is a short follow-up to that earlier task.",
            "criteria": {
                "plan": "Design, architecture, or a strategy before implementation.",
                "implement": "Write or change code to add or finish behavior.",
                "debug": "Find and fix a defect, failure, or unexpected behavior.",
                "refactor": "Restructure existing code without changing intended behavior.",
                "review": "Review code, a diff, or a design for correctness or risk.",
                "research": "Investigate an unknown API, codebase area, or approach.",
                "explain": "Explain existing code or a concept without changing it.",
                "chat": "A short conversational request that is not a coding task."
            }
        },
        "complexity": {
            "type": "score",
            "instructions": "How difficult is state.task?",
            "criteria": [
                "Trivial: a tiny, local, already-specified change or question.",
                "Routine: a bounded change with a known approach.",
                "Involved: several parts, careful reasoning, or non-obvious edge cases.",
                "Architectural: cross-cutting design, broad coordination, or a new subsystem."
            ]
        },
        "deep_preparation": {
            "type": "noul",
            "instructions": "Does successful completion require deep preparation: open-ended investigation, an unknown design, or a strategy that is not already specified?",
            "criteria": {
                "true": "The approach, root cause, or design still has to be discovered.",
                "false": "The task can follow a known, bounded procedure."
            }
        },
        "consequence": {
            "type": "noul",
            "instructions": "Would an incorrect result have significant consequences, such as broad regressions, compatibility breakage, security issues, or data loss?",
            "criteria": {
                "true": "A wrong change could cause broad regressions, breakage, security issues, or data loss.",
                "false": "A wrong change would stay local and easy to undo."
            }
        }
    })
}

fn choice(label: &'static str, model: &str) -> RouteChoice {
    RouteChoice {
        label,
        model: model.to_string(),
        note: None,
    }
}

fn fallback_choice(model: &str, note: Option<&'static str>) -> RouteChoice {
    RouteChoice {
        label: "pinned",
        model: model.to_string(),
        note,
    }
}

fn json_f64(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_str().and_then(|text| text.parse().ok()))
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let end = text
        .char_indices()
        .nth(max_chars)
        .map(|(idx, _)| idx)
        .unwrap_or(text.len());
    format!("{}…", &text[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tiers() -> TierModels {
        TierModels {
            prep: "prep-model".into(),
            code: "code-model".into(),
            code_hard: "code-hard-model".into(),
            quick: "quick-model".into(),
        }
    }

    fn judgment(
        kind: &str,
        confidence: f64,
        complexity: f64,
        deep_preparation: f64,
        consequence: f64,
    ) -> Judgment {
        Judgment {
            task_kind: kind.into(),
            task_kind_confidence: confidence,
            complexity,
            deep_preparation,
            consequence,
            cost_usd: None,
        }
    }

    fn sample_model(id: &str, supports_tools: bool) -> OpenRouterModel {
        OpenRouterModel {
            id: id.into(),
            name: id.into(),
            description: String::new(),
            context_length: None,
            supports_tools,
            prompt_price: None,
            completion_price: None,
        }
    }

    #[test]
    fn parses_decision_fixture() {
        let body = json!({
            "answers": {
                "task_kind": {
                    "type": "choice",
                    "choice": "Implement",
                    "confidence": 0.82,
                    "probabilities": { "implement": 0.82 }
                },
                "complexity": { "type": "score", "score": 2.4, "confidence": 0.7 },
                "deep_preparation": { "type": "noul", "noul": "0.2" },
                "consequence": { "type": "noul", "noul": 0.1 }
            },
            "usage": { "cost": 0.00012 }
        });
        let parsed = parse_judgment(&body).unwrap();
        assert_eq!(parsed.task_kind, "implement");
        assert_eq!(parsed.task_kind_confidence, 0.82);
        assert_eq!(parsed.complexity, 2.4);
        assert_eq!(parsed.deep_preparation, 0.2);
        assert_eq!(parsed.consequence, 0.1);
        assert_eq!(parsed.cost_usd, Some(0.00012));
    }

    #[test]
    fn routes_preparation_and_coding_difficulty() {
        let tiers = tiers();
        let pinned = "pinned-model";
        assert_eq!(
            select_tier(&judgment("plan", 0.9, 0.2, 0.1, 0.1), &tiers, pinned).label,
            "prep"
        );
        assert_eq!(
            select_tier(&judgment("review", 0.9, 1.0, 0.1, 0.1), &tiers, pinned).model,
            "prep-model"
        );
        assert_eq!(
            select_tier(&judgment("research", 0.8, 0.4, 0.2, 0.1), &tiers, pinned).label,
            "prep"
        );

        let routine = select_tier(&judgment("implement", 0.9, 1.0, 0.2, 0.1), &tiers, pinned);
        assert_eq!(routine.label, "code");
        assert_eq!(routine.model, "code-model");

        assert_eq!(
            select_tier(&judgment("debug", 0.9, 0.2, 0.1, 0.1), &tiers, pinned).label,
            "code"
        );
        assert_eq!(
            select_tier(&judgment("refactor", 0.9, 2.1, 0.1, 0.1), &tiers, pinned).model,
            "code-hard-model"
        );
        assert_eq!(
            select_tier(&judgment("implement", 0.9, 0.4, 0.8, 0.1), &tiers, pinned).label,
            "code-hard"
        );
        assert_eq!(
            select_tier(&judgment("implement", 0.9, 0.4, 0.1, 0.8), &tiers, pinned).label,
            "code-hard"
        );
    }

    #[test]
    fn routes_chat_and_falls_back_when_unsure() {
        let tiers = tiers();
        let pinned = "pinned-model";
        let quick = select_tier(&judgment("chat", 0.9, 0.2, 0.1, 0.1), &tiers, pinned);
        assert_eq!(quick.label, "quick");
        assert_eq!(quick.model, "quick-model");
        assert_eq!(quick.status_line(), "jev → quick quick-model");

        assert_eq!(
            select_tier(&judgment("explain", 0.9, 2.2, 0.1, 0.1), &tiers, pinned).label,
            "prep"
        );
        assert_eq!(
            select_tier(&judgment("explain", 0.9, 0.2, 0.7, 0.1), &tiers, pinned).model,
            "prep-model"
        );

        let unsure = select_tier(&judgment("plan", 0.44, 3.0, 0.9, 0.9), &tiers, pinned);
        assert!(unsure.is_fallback());
        assert_eq!(unsure.model, pinned);
        assert_eq!(unsure.note, Some("low confidence"));
        assert_eq!(
            unsure.status_line(),
            "jev → pinned pinned-model (low confidence)"
        );

        let unknown = select_tier(&judgment("other", 0.9, 0.1, 0.1, 0.1), &tiers, pinned);
        assert_eq!(unknown.note, Some("unrecognized task"));
    }

    #[test]
    fn short_follow_up_includes_prior_task() {
        let short = task_state("do it", Some("Add a parser for the config file"));
        assert_eq!(short["task"], "do it");
        assert_eq!(short["prior_task"], "Add a parser for the config file");

        let long_task = "x".repeat(FOLLOW_UP_MAX_CHARS + 1);
        let long = task_state(&long_task, Some("earlier task"));
        assert!(long.get("prior_task").is_none());

        let exact = "y".repeat(FOLLOW_UP_MAX_CHARS);
        let boundary = task_state(&exact, Some("earlier"));
        assert_eq!(boundary["prior_task"], "earlier");
    }

    #[test]
    fn catalog_rejects_stale_and_tool_less_models() {
        let models = vec![
            sample_model("code-model", true),
            sample_model("no-tools", false),
        ];
        assert_eq!(catalog_fit(&models, "code-model"), CatalogFit::Usable);
        assert_eq!(catalog_fit(&models, "no-tools"), CatalogFit::NoTools);
        assert_eq!(catalog_fit(&models, "missing"), CatalogFit::Missing);
        assert_eq!(
            catalog_fit(&models, "~anthropic/claude-opus-latest"),
            CatalogFit::Usable
        );

        let selected = choice("code", "missing");
        let fallback = apply_catalog(selected, CatalogFit::Missing, "pinned-model");
        assert_eq!(fallback.model, "pinned-model");
        assert_eq!(fallback.note, Some("model not in catalog"));

        let no_tools = apply_catalog(
            choice("code", "no-tools"),
            CatalogFit::NoTools,
            "pinned-model",
        );
        assert_eq!(no_tools.note, Some("model does not support tools"));

        let kept = apply_catalog(
            choice("prep", "~anthropic/claude-opus-latest"),
            CatalogFit::Unchecked,
            "pinned-model",
        );
        assert_eq!(kept.model, "~anthropic/claude-opus-latest");
    }

    #[test]
    fn questions_cover_the_four_judgments() {
        let questions = questions();
        for key in ["task_kind", "complexity", "deep_preparation", "consequence"] {
            assert!(questions.get(key).is_some(), "missing {key}");
        }
        assert_eq!(questions["task_kind"]["type"], "choice");
        assert_eq!(questions["complexity"]["type"], "score");
        assert_eq!(questions["deep_preparation"]["type"], "noul");
        assert_eq!(questions["consequence"]["type"], "noul");
        assert!(DECISIONS_URL.contains("openrouter.ai/api/alpha/decisions"));
        assert_eq!(JEV_MODEL, "~typesafe/jev-latest");
    }

    #[test]
    fn groq_routes_difficulty_using_its_own_models() {
        let tiers = TierModels {
            prep: "openai/gpt-oss-120b".into(),
            code: "openai/gpt-oss-20b".into(),
            code_hard: "openai/gpt-oss-120b".into(),
            quick: "openai/gpt-oss-20b".into(),
        };
        let models: Vec<GroqModel> = serde_json::from_value(json!([
            {"id":"openai/gpt-oss-120b"}, {"id":"openai/gpt-oss-20b"}
        ]))
        .unwrap();
        for (kind, complexity, expected) in [
            ("chat", 0.1, "openai/gpt-oss-20b"),
            ("implement", 1.0, "openai/gpt-oss-20b"),
            ("debug", 2.5, "openai/gpt-oss-120b"),
            ("review", 1.0, "openai/gpt-oss-120b"),
        ] {
            let choice = select_tier(&judgment(kind, 0.9, complexity, 0.1, 0.1), &tiers, "pinned");
            assert_eq!(
                apply_groq_catalog(choice, Some(&models), "pinned").model,
                expected
            );
        }
        let choice = select_tier(&judgment("chat", 0.1, 0.1, 0.1, 0.1), &tiers, "pinned");
        assert_eq!(
            apply_groq_catalog(choice, Some(&models), "pinned").model,
            "pinned"
        );
    }

    #[test]
    fn groq_catalog_rejects_aliases_inactive_unknown_and_missing_models() {
        let models: Vec<GroqModel> = serde_json::from_value(json!([
            {"id":"openai/gpt-oss-20b", "active":false},
            {"id":"whisper-large-v3"}, {"id":"unknown/model"}
        ]))
        .unwrap();
        for id in [
            "~anthropic/claude-opus-latest",
            "openai/gpt-oss-20b",
            "whisper-large-v3",
            "unknown/model",
            "missing",
        ] {
            let route = apply_groq_catalog(choice("code", id), Some(&models), "groq-pinned");
            assert!(route.is_fallback());
            assert_eq!(route.model, "groq-pinned");
        }
        let route = apply_groq_catalog(choice("code", "openai/gpt-oss-20b"), None, "groq-pinned");
        assert_eq!(route.note, Some("Groq catalog unavailable"));
    }

    #[tokio::test]
    async fn missing_decision_key_does_not_make_a_request() {
        let error = classify_turn(" ", "task", None, &tiers(), "pinned")
            .await
            .unwrap_err();
        assert!(error.to_string().contains("OpenRouter API key"));
    }

    #[tokio::test]
    async fn jev_decision_uses_only_the_openrouter_credential() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!(
            "http://{}/api/alpha/decisions",
            listener.local_addr().unwrap()
        );
        let server = tokio::spawn(async move {
            tokio::time::timeout(Duration::from_secs(5), async move {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                let mut buf = [0; 4096];
                let (headers, payload) = loop {
                    let count = socket.read(&mut buf).await.unwrap();
                    assert!(count > 0);
                    request.extend_from_slice(&buf[..count]);
                    if let Some(end) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&request[..end]).to_string();
                        let length: usize = headers.lines().find_map(|line| {
                            let (key, value) = line.split_once(':')?;
                            key.eq_ignore_ascii_case("content-length").then(|| value.trim().parse().unwrap())
                        }).unwrap();
                        if request.len() >= end + 4 + length {
                            break (headers, serde_json::from_slice::<Value>(&request[end + 4..end + 4 + length]).unwrap());
                        }
                    }
                };
                let body = json!({"answers": {
                    "task_kind":{"choice":"implement", "confidence":0.9}, "complexity":{"score":1.0},
                    "deep_preparation":{"noul":0.1}, "consequence":{"noul":0.1}
                }}).to_string();
                socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
                (headers, payload)
            }).await.unwrap()
        });
        let decision = judge_at(
            "openrouter-test-key",
            &task_state("do it", Some("add a parser")),
            &endpoint,
        )
        .await
        .unwrap();
        assert_eq!(decision.task_kind, "implement");
        let (headers, payload) = server.await.unwrap();
        assert!(headers
            .to_ascii_lowercase()
            .contains("authorization: bearer openrouter-test-key"));
        assert!(!headers.contains("groq-test-key"));
        assert_eq!(payload["model"], JEV_MODEL);
        assert_eq!(payload["state"]["prior_task"], "add a parser");
        assert!(payload.get("messages").is_none());
    }
}
