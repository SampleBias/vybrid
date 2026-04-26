#![allow(dead_code)]

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{oneshot, Mutex};
use tokio::task::JoinHandle;
use tokio::time::timeout;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(8);
const INIT_TIMEOUT: Duration = Duration::from_secs(20);
const MAX_LSP_OUTPUT_BYTES: usize = 96 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RustLspState {
    Off,
    Connecting,
    Connected,
    Error,
}

#[derive(Debug, Clone)]
pub struct RustLspStatus {
    pub state: RustLspState,
    pub command: String,
    pub root: Option<PathBuf>,
    pub message: Option<String>,
}

impl RustLspStatus {
    fn off() -> Self {
        Self {
            state: RustLspState::Off,
            command: "rust-analyzer".to_string(),
            root: None,
            message: None,
        }
    }

    pub fn summary(&self) -> String {
        let state = match self.state {
            RustLspState::Off => "off",
            RustLspState::Connecting => "connecting",
            RustLspState::Connected => "connected",
            RustLspState::Error => "error",
        };
        let root = self
            .root
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "<default>".to_string());
        let message = self
            .message
            .as_ref()
            .map(|m| format!("\nmessage: {m}"))
            .unwrap_or_default();
        format!(
            "Rust LSP: {state}\ncommand: {}\nroot: {root}{message}",
            self.command
        )
    }
}

struct RustLspProcess {
    stdin: Arc<Mutex<ChildStdin>>,
    child: Child,
    reader_task: JoinHandle<()>,
    stderr_task: JoinHandle<()>,
}

struct RustLspInner {
    status: Arc<Mutex<RustLspStatus>>,
    process: Mutex<Option<RustLspProcess>>,
    pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value, String>>>>>,
    diagnostics: Arc<Mutex<HashMap<String, Value>>>,
    opened_documents: Mutex<HashSet<String>>,
    next_id: AtomicU64,
}

#[derive(Clone)]
pub struct RustLspManager {
    inner: Arc<RustLspInner>,
}

#[derive(Debug, Clone)]
pub enum RustLspOperation {
    Status,
    Diagnostics,
    Hover,
    Definition,
    References,
    DocumentSymbols,
    Completion,
    CodeActions,
    Formatting,
}

impl RustLspOperation {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "status" => Some(Self::Status),
            "diagnostics" | "diagnostic" => Some(Self::Diagnostics),
            "hover" => Some(Self::Hover),
            "definition" | "goto_definition" => Some(Self::Definition),
            "references" => Some(Self::References),
            "document_symbols" | "symbols" => Some(Self::DocumentSymbols),
            "completion" | "completions" => Some(Self::Completion),
            "code_actions" | "code_action" => Some(Self::CodeActions),
            "formatting" | "format" => Some(Self::Formatting),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct RustLspQuery {
    pub operation: RustLspOperation,
    pub file_path: Option<String>,
    pub line: Option<u32>,
    pub character: Option<u32>,
}

impl RustLspManager {
    pub fn new(command: impl Into<String>, root: Option<PathBuf>) -> Self {
        let mut status = RustLspStatus::off();
        status.command = command.into();
        status.root = root;
        Self {
            inner: Arc::new(RustLspInner {
                status: Arc::new(Mutex::new(status)),
                process: Mutex::new(None),
                pending: Arc::new(Mutex::new(HashMap::new())),
                diagnostics: Arc::new(Mutex::new(HashMap::new())),
                opened_documents: Mutex::new(HashSet::new()),
                next_id: AtomicU64::new(1),
            }),
        }
    }

    pub async fn status(&self) -> RustLspStatus {
        self.inner.status.lock().await.clone()
    }

    pub async fn is_connected(&self) -> bool {
        self.status().await.state == RustLspState::Connected
    }

    pub async fn connect(&self, command: &str, root: PathBuf) -> Result<()> {
        self.disconnect().await.ok();
        self.set_status(RustLspState::Connecting, command, Some(root.clone()), None)
            .await;

        let mut parts = command.split_whitespace();
        let program = parts
            .next()
            .filter(|p| !p.trim().is_empty())
            .unwrap_or("rust-analyzer");
        let args = parts.collect::<Vec<_>>();

        let mut cmd = Command::new(program);
        cmd.args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        let mut child = match cmd.spawn() {
            Ok(child) => child,
            Err(e) => {
                let msg = format!("failed to spawn {program}: {e}");
                self.set_status(RustLspState::Error, command, Some(root), Some(msg.clone()))
                    .await;
                return Err(anyhow!(msg));
            }
        };

        let stdin =
            Arc::new(Mutex::new(child.stdin.take().ok_or_else(|| {
                anyhow!("rust-analyzer stdin was not available")
            })?));
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("rust-analyzer stdout was not available"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| anyhow!("rust-analyzer stderr was not available"))?;

        let reader_task = spawn_reader(
            stdout,
            self.inner.pending.clone(),
            self.inner.diagnostics.clone(),
            self.inner.status.clone(),
        );
        let stderr_task = tokio::spawn(async move {
            let mut reader = BufReader::new(stderr).lines();
            while matches!(reader.next_line().await, Ok(Some(_))) {}
        });

        {
            let mut process = self.inner.process.lock().await;
            *process = Some(RustLspProcess {
                stdin,
                child,
                reader_task,
                stderr_task,
            });
        }

        let root_uri = path_to_file_uri(&root)?;
        let init = json!({
            "processId": std::process::id(),
            "rootPath": root.to_string_lossy(),
            "rootUri": root_uri,
            "capabilities": {
                "textDocument": {
                    "hover": { "dynamicRegistration": false, "contentFormat": ["markdown", "plaintext"] },
                    "definition": { "dynamicRegistration": false },
                    "references": { "dynamicRegistration": false },
                    "documentSymbol": { "dynamicRegistration": false },
                    "completion": { "dynamicRegistration": false },
                    "codeAction": { "dynamicRegistration": false },
                    "formatting": { "dynamicRegistration": false },
                    "publishDiagnostics": { "relatedInformation": true }
                },
                "workspace": {
                    "workspaceFolders": true,
                    "configuration": false
                }
            },
            "workspaceFolders": [{
                "uri": root_uri,
                "name": root.file_name().and_then(|n| n.to_str()).unwrap_or("workspace")
            }]
        });

        match timeout(INIT_TIMEOUT, self.request("initialize", init)).await {
            Ok(Ok(_)) => {
                self.notify("initialized", json!({})).await?;
                self.set_status(RustLspState::Connected, command, Some(root), None)
                    .await;
                Ok(())
            }
            Ok(Err(e)) => {
                self.set_status(
                    RustLspState::Error,
                    command,
                    Some(root),
                    Some(format!("initialize failed: {e}")),
                )
                .await;
                Err(e)
            }
            Err(_) => {
                let msg = "initialize timed out".to_string();
                self.set_status(RustLspState::Error, command, Some(root), Some(msg.clone()))
                    .await;
                Err(anyhow!(msg))
            }
        }
    }

    pub async fn disconnect(&self) -> Result<()> {
        if self.inner.process.lock().await.is_none() {
            self.set_state_only(RustLspState::Off).await;
            return Ok(());
        }

        let _ = self.request("shutdown", Value::Null).await;
        let _ = self.notify("exit", Value::Null).await;

        let mut process = self.inner.process.lock().await;
        if let Some(mut process) = process.take() {
            process.reader_task.abort();
            process.stderr_task.abort();
            let _ = process.child.kill().await;
        }
        self.inner.pending.lock().await.clear();
        self.inner.diagnostics.lock().await.clear();
        self.inner.opened_documents.lock().await.clear();
        self.set_state_only(RustLspState::Off).await;
        Ok(())
    }

    pub async fn restart(&self, command: &str, root: PathBuf) -> Result<()> {
        self.disconnect().await.ok();
        self.connect(command, root).await
    }

    pub async fn query(&self, query: RustLspQuery) -> Result<String> {
        if !self.is_connected().await {
            return Ok(self.status().await.summary());
        }

        match query.operation {
            RustLspOperation::Status => Ok(self.status().await.summary()),
            RustLspOperation::Diagnostics => self.diagnostics(query.file_path.as_deref()).await,
            RustLspOperation::Hover => {
                let (uri, position) = self.require_text_position(&query).await?;
                self.call_text_position("textDocument/hover", uri, position)
                    .await
            }
            RustLspOperation::Definition => {
                let (uri, position) = self.require_text_position(&query).await?;
                self.call_text_position("textDocument/definition", uri, position)
                    .await
            }
            RustLspOperation::References => {
                let (uri, position) = self.require_text_position(&query).await?;
                let result = self
                    .request(
                        "textDocument/references",
                        json!({
                            "textDocument": { "uri": uri },
                            "position": position,
                            "context": { "includeDeclaration": true }
                        }),
                    )
                    .await?;
                Ok(truncate_json("references", &result))
            }
            RustLspOperation::DocumentSymbols => {
                let uri = self
                    .require_text_document(query.file_path.as_deref())
                    .await?;
                let result = self
                    .request(
                        "textDocument/documentSymbol",
                        json!({ "textDocument": { "uri": uri } }),
                    )
                    .await?;
                Ok(truncate_json("document symbols", &result))
            }
            RustLspOperation::Completion => {
                let (uri, position) = self.require_text_position(&query).await?;
                self.call_text_position("textDocument/completion", uri, position)
                    .await
            }
            RustLspOperation::CodeActions => {
                let (uri, position) = self.require_text_position(&query).await?;
                let result = self
                    .request(
                        "textDocument/codeAction",
                        json!({
                            "textDocument": { "uri": uri },
                            "range": { "start": position, "end": position },
                            "context": { "diagnostics": [] }
                        }),
                    )
                    .await?;
                Ok(truncate_json("code actions", &result))
            }
            RustLspOperation::Formatting => {
                let uri = self
                    .require_text_document(query.file_path.as_deref())
                    .await?;
                let result = self
                    .request(
                        "textDocument/formatting",
                        json!({
                            "textDocument": { "uri": uri },
                            "options": { "tabSize": 4, "insertSpaces": true }
                        }),
                    )
                    .await?;
                Ok(truncate_json("formatting edits", &result))
            }
        }
    }

    async fn require_text_position(&self, query: &RustLspQuery) -> Result<(String, Value)> {
        let uri = self
            .require_text_document(query.file_path.as_deref())
            .await?;
        let line = query
            .line
            .ok_or_else(|| anyhow!("rust_lsp_query: line is required for this operation"))?;
        let character = query
            .character
            .ok_or_else(|| anyhow!("rust_lsp_query: character is required for this operation"))?;
        Ok((uri, json!({ "line": line, "character": character })))
    }

    async fn require_text_document(&self, file_path: Option<&str>) -> Result<String> {
        let file_path =
            file_path.ok_or_else(|| anyhow!("rust_lsp_query: file_path is required"))?;
        self.open_document(file_path).await
    }

    async fn call_text_position(
        &self,
        method: &str,
        uri: String,
        position: Value,
    ) -> Result<String> {
        let result = self
            .request(
                method,
                json!({
                    "textDocument": { "uri": uri },
                    "position": position
                }),
            )
            .await?;
        Ok(truncate_json(method, &result))
    }

    async fn open_document(&self, file_path: &str) -> Result<String> {
        let path = normalize_path(file_path);
        let uri = path_to_file_uri(&path)?;
        if self.inner.opened_documents.lock().await.contains(&uri) {
            return Ok(uri);
        }
        let text = tokio::fs::read_to_string(&path)
            .await
            .with_context(|| format!("rust_lsp_query: failed to read {}", path.display()))?;
        self.notify(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": uri,
                    "languageId": "rust",
                    "version": 1,
                    "text": text
                }
            }),
        )
        .await?;
        self.inner.opened_documents.lock().await.insert(uri.clone());
        Ok(uri)
    }

    async fn diagnostics(&self, file_path: Option<&str>) -> Result<String> {
        let uri = if let Some(path) = file_path {
            Some(self.open_document(path).await?)
        } else {
            None
        };
        let diagnostics = self.inner.diagnostics.lock().await;
        if diagnostics.is_empty() {
            return Ok("No Rust LSP diagnostics have been published yet.".to_string());
        }
        let selected = if let Some(uri) = uri {
            diagnostics
                .get(&uri)
                .cloned()
                .unwrap_or_else(|| json!({ "uri": uri, "diagnostics": [] }))
        } else {
            json!(diagnostics
                .iter()
                .map(|(uri, value)| json!({ "uri": uri, "diagnostics": value }))
                .collect::<Vec<_>>())
        };
        Ok(truncate_json("diagnostics", &selected))
    }

    async fn request(&self, method: &str, params: Value) -> Result<Value> {
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        let request = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params
        });
        let (tx, rx) = oneshot::channel();
        self.inner.pending.lock().await.insert(id, tx);
        if let Err(e) = self.write_message(&request).await {
            self.inner.pending.lock().await.remove(&id);
            return Err(e);
        }
        match timeout(REQUEST_TIMEOUT, rx).await {
            Ok(Ok(Ok(value))) => Ok(value),
            Ok(Ok(Err(message))) => Err(anyhow!(message)),
            Ok(Err(_)) => Err(anyhow!("rust-analyzer response channel closed")),
            Err(_) => {
                self.inner.pending.lock().await.remove(&id);
                Err(anyhow!("{method} timed out after {:?}", REQUEST_TIMEOUT))
            }
        }
    }

    async fn notify(&self, method: &str, params: Value) -> Result<()> {
        self.write_message(&json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params
        }))
        .await
    }

    async fn write_message(&self, value: &Value) -> Result<()> {
        let body = serde_json::to_vec(value)?;
        let header = format!("Content-Length: {}\r\n\r\n", body.len());
        let process = self.inner.process.lock().await;
        let stdin = process
            .as_ref()
            .map(|p| p.stdin.clone())
            .ok_or_else(|| anyhow!("Rust LSP is not connected"))?;
        drop(process);

        let mut stdin = stdin.lock().await;
        stdin.write_all(header.as_bytes()).await?;
        stdin.write_all(&body).await?;
        stdin.flush().await?;
        Ok(())
    }

    async fn set_status(
        &self,
        state: RustLspState,
        command: &str,
        root: Option<PathBuf>,
        message: Option<String>,
    ) {
        let mut status = self.inner.status.lock().await;
        status.state = state;
        status.command = command.to_string();
        status.root = root;
        status.message = message;
    }

    async fn set_state_only(&self, state: RustLspState) {
        let mut status = self.inner.status.lock().await;
        status.state = state;
        if state != RustLspState::Error {
            status.message = None;
        }
    }
}

fn spawn_reader<R>(
    stdout: R,
    pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value, String>>>>>,
    diagnostics: Arc<Mutex<HashMap<String, Value>>>,
    status: Arc<Mutex<RustLspStatus>>,
) -> JoinHandle<()>
where
    R: AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut reader = BufReader::new(stdout);
        loop {
            let message = match read_lsp_message(&mut reader).await {
                Ok(Some(message)) => message,
                Ok(None) => break,
                Err(e) => {
                    let mut status = status.lock().await;
                    status.state = RustLspState::Error;
                    status.message = Some(format!("reader error: {e}"));
                    break;
                }
            };

            if let Some(id) = message.get("id").and_then(Value::as_u64) {
                if let Some(sender) = pending.lock().await.remove(&id) {
                    let result = if let Some(error) = message.get("error") {
                        Err(error.to_string())
                    } else {
                        Ok(message.get("result").cloned().unwrap_or(Value::Null))
                    };
                    let _ = sender.send(result);
                }
                continue;
            }

            if message.get("method").and_then(Value::as_str)
                == Some("textDocument/publishDiagnostics")
            {
                if let Some(params) = message.get("params") {
                    if let Some(uri) = params.get("uri").and_then(Value::as_str) {
                        let value = params
                            .get("diagnostics")
                            .cloned()
                            .unwrap_or_else(|| json!([]));
                        diagnostics.lock().await.insert(uri.to_string(), value);
                    }
                }
            }
        }
    })
}

async fn read_lsp_message<R>(reader: &mut BufReader<R>) -> Result<Option<Value>>
where
    R: AsyncRead + Unpin,
{
    let mut content_length = None;
    loop {
        let mut line = String::new();
        let bytes = reader.read_line(&mut line).await?;
        if bytes == 0 {
            return Ok(None);
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            break;
        }
        if let Some(value) = trimmed.strip_prefix("Content-Length:") {
            content_length = Some(value.trim().parse::<usize>()?);
        }
    }

    let len = content_length.ok_or_else(|| anyhow!("missing Content-Length header"))?;
    let mut body = vec![0u8; len];
    reader.read_exact(&mut body).await?;
    let value = serde_json::from_slice(&body)?;
    Ok(Some(value))
}

fn normalize_path(file_path: &str) -> PathBuf {
    let trimmed = file_path.trim();
    if let Some(stripped) = trimmed.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(stripped);
        }
    }
    let path = PathBuf::from(trimmed);
    if path.is_absolute() {
        path
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    }
}

fn path_to_file_uri(path: &Path) -> Result<String> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let path = absolute
        .to_str()
        .ok_or_else(|| anyhow!("path is not valid UTF-8: {}", absolute.display()))?;
    Ok(format!("file://{}", percent_encode_path(path)))
}

fn percent_encode_path(path: &str) -> String {
    let mut out = String::new();
    for byte in path.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

fn truncate_json(label: &str, value: &Value) -> String {
    let text = serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string());
    if text.len() <= MAX_LSP_OUTPUT_BYTES {
        return format!("Rust LSP {label}:\n{text}");
    }
    let head = text
        .char_indices()
        .take_while(|(idx, _)| *idx < MAX_LSP_OUTPUT_BYTES / 2)
        .last()
        .map(|(idx, ch)| &text[..idx + ch.len_utf8()])
        .unwrap_or("");
    let tail_start = text.len().saturating_sub(MAX_LSP_OUTPUT_BYTES / 2);
    let tail = &text[tail_start..];
    format!(
        "Rust LSP {label}:\n{head}\n\n[LSP output truncated: {} bytes omitted]\n\n{tail}",
        text.len().saturating_sub(head.len() + tail.len())
    )
}
