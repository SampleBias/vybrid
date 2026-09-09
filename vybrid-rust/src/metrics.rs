//! Optional local JSONL metrics. Never records prompts, tool arguments, or secrets.
use crate::client::groq::Usage;
use serde_json::json;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Instant;

static WRITER: Mutex<()> = Mutex::new(());

fn destination() -> Option<PathBuf> {
    std::env::var_os("VYBRID_METRICS_FILE")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

fn emit(path: Option<&PathBuf>, record: serde_json::Value) {
    let Some(path) = path else {
        return;
    };
    let Ok(_guard) = WRITER.lock() else {
        return;
    };
    let write = || -> std::io::Result<()> {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        writeln!(file, "{record}")
    };
    if let Err(error) = write() {
        eprintln!("Metrics write failed: {error}");
    }
}

pub struct RequestMetrics {
    path: Option<PathBuf>,
    started: Instant,
    first_chunk_ms: Option<u128>,
    model: String,
    task_id: Option<String>,
    estimated_tokens: u32,
    usage: Option<Usage>,
    status: &'static str,
    finish_reason: Option<String>,
    phase: &'static str,
}

impl RequestMetrics {
    pub fn new(model: &str, task_id: Option<&str>, estimate: u32, phase: &'static str) -> Self {
        Self {
            path: destination(),
            started: Instant::now(),
            first_chunk_ms: None,
            model: model.into(),
            task_id: task_id.map(str::to_owned),
            estimated_tokens: estimate,
            usage: None,
            status: "failed_or_cancelled",
            finish_reason: None,
            phase,
        }
    }
    pub fn chunk(&mut self, usage: Option<Usage>, finish: Option<&str>) {
        self.first_chunk_ms
            .get_or_insert_with(|| self.started.elapsed().as_millis());
        if usage.is_some() {
            self.usage = usage;
        }
        if let Some(finish) = finish {
            self.finish_reason = Some(finish.into());
        }
    }
    pub fn complete(&mut self) {
        self.status = match self.finish_reason.as_deref() {
            Some("stop" | "tool_calls") => "completed",
            _ => "incomplete",
        };
    }
}

impl Drop for RequestMetrics {
    fn drop(&mut self) {
        if self.path.is_none() {
            return;
        }
        emit(
            self.path.as_ref(),
            json!({
                "event": "request", "timestamp": chrono::Utc::now().to_rfc3339(),
                "task_id": self.task_id, "model": self.model, "phase": self.phase,
                "elapsed_ms": self.started.elapsed().as_millis(), "first_chunk_ms": self.first_chunk_ms,
                "estimated_tokens": self.estimated_tokens, "status": self.status, "finish_reason": self.finish_reason,
                "prompt_tokens": self.usage.and_then(|u| u.prompt_tokens),
                "cached_tokens": self.usage.and_then(|u| u.prompt_tokens_details.and_then(|d| d.cached_tokens)),
                "completion_tokens": self.usage.and_then(|u| u.completion_tokens),
                "total_tokens": self.usage.and_then(|u| u.total_tokens)
            }),
        );
    }
}

pub struct TaskMetrics {
    pub id: String,
    started: Instant,
    path: Option<PathBuf>,
}
impl Default for TaskMetrics {
    fn default() -> Self {
        Self::new()
    }
}
impl TaskMetrics {
    pub fn new() -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            started: Instant::now(),
            path: destination(),
        }
    }
    pub fn finish(self, success: bool) {
        emit(
            self.path.as_ref(),
            json!({ "event": "task", "task_id": self.id, "elapsed_ms": self.started.elapsed().as_millis(), "returned_successfully": success }),
        );
    }
}

pub fn tool(task_id: Option<&str>, name: &str, started: Instant, success: bool, bytes: usize) {
    let path = destination();
    if path.is_some() {
        emit(
            path.as_ref(),
            json!({ "event": "tool", "task_id": task_id, "tool": name, "elapsed_ms": started.elapsed().as_millis(), "success": success, "returned_bytes": bytes }),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn failed_attempts_keep_reported_usage_and_missing_usage_is_null() {
        let path =
            std::env::temp_dir().join(format!("vybrid-metrics-{}.jsonl", uuid::Uuid::new_v4()));
        {
            let mut record = RequestMetrics::new("test-model", Some("task"), 100, "generation");
            record.path = Some(path.clone());
            record.chunk(
                Some(Usage {
                    prompt_tokens: Some(100),
                    completion_tokens: Some(20),
                    total_tokens: Some(120),
                    prompt_tokens_details: None,
                }),
                Some("length"),
            );
            record.complete();
        }
        {
            let mut record = RequestMetrics::new("test-model", Some("task"), 100, "generation");
            record.path = Some(path.clone());
        }
        let lines = std::fs::read_to_string(&path).unwrap();
        let records: Vec<serde_json::Value> = lines
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(records[0]["status"], "incomplete");
        assert_eq!(records[0]["total_tokens"], 120);
        assert!(records[0]["cached_tokens"].is_null());
        assert_eq!(records[1]["status"], "failed_or_cancelled");
        assert!(records[1]["total_tokens"].is_null());
        std::fs::remove_file(path).unwrap();
    }
}
