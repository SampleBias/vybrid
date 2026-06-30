#![allow(dead_code)]

use console::{style, Term};
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::lsp::{RustLspState, RustLspStatus};

/// Approximate `openai/gpt-oss-120b` context window (tokens). Used only for the CLI meter.
pub const CONTEXT_WINDOW_TOKENS: u32 = 131_072;

/// Rotating circle spinner on stderr until [`SpinnerGuard::finish`] — shows activity while the LLM
/// connects and before the first streamed chunk (thinking / TTFB). Label is e.g. `groq` or `local`.
pub struct SpinnerGuard {
    stop: Arc<AtomicBool>,
    handle: Option<tokio::task::JoinHandle<()>>,
}

impl SpinnerGuard {
    pub fn new(label: impl Into<String>) -> Self {
        let label = label.into();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_clone = stop.clone();
        let handle = tokio::spawn(async move {
            let frames = ["◐", "◓", "◑", "◒"];
            let mut i = 0u32;
            while !stop_clone.load(Ordering::Relaxed) {
                eprint!(
                    "\r\x1b[2K{} {} {}",
                    style(&label).dim(),
                    style("·").dim(),
                    style(frames[(i % 4) as usize]).cyan()
                );
                let _ = std::io::stderr().flush();
                i += 1;
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
            eprint!("\r\x1b[2K");
            let _ = std::io::stderr().flush();
        });
        Self {
            stop,
            handle: Some(handle),
        }
    }

    pub async fn finish(&mut self) {
        if let Some(h) = self.handle.take() {
            self.stop.store(true, Ordering::Relaxed);
            let _ = h.await;
        }
    }
}

/// Eight filled/empty circles as a discrete ring plus rough token counts.
pub fn format_context_ring(estimated_tokens: u32, max_tokens: u32) -> String {
    let pct = if max_tokens == 0 {
        0.0
    } else {
        (estimated_tokens.min(max_tokens) as f64 / max_tokens as f64 * 100.0).min(100.0)
    };
    const SEGMENTS: usize = 8;
    let filled = ((pct / 100.0) * SEGMENTS as f64).round() as usize;
    let filled = filled.min(SEGMENTS);
    let mut ring = String::with_capacity(SEGMENTS);
    for i in 0..SEGMENTS {
        if i < filled {
            ring.push('●');
        } else {
            ring.push('○');
        }
    }
    format!(
        "ctx {}  {:>5.1}%  ~{} / {} tok",
        ring,
        pct,
        format_tokens_short(estimated_tokens),
        format_tokens_short(max_tokens)
    )
}

fn format_tokens_short(n: u32) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1000.0)
    } else {
        n.to_string()
    }
}

/// One dim line: context fill vs active request budget (heuristic; see `Conversation::estimate_context_tokens`).
pub fn print_context_status_line(
    estimated_tokens: u32,
    request_budget: u32,
    max_completion_tokens: u32,
    model: &str,
    reasoning_effort: Option<&str>,
    rust_lsp: &RustLspStatus,
) {
    let thinking = crate::config::format_thinking_indicator(model, reasoning_effort);
    let line = format!(
        "{}  out {}  {}  {}",
        format_context_ring(estimated_tokens, request_budget),
        format_tokens_short(max_completion_tokens),
        thinking,
        format_rust_lsp_indicator(rust_lsp)
    );
    let pct = estimated_tokens as f64 / request_budget.max(1) as f64;
    if pct >= 0.85 {
        println!("{}", style(line).yellow().dim());
    } else if pct >= 0.65 {
        println!("{}", style(line).cyan().dim());
    } else {
        println!("{}", style(line).dim());
    }
}

/// Dim per-response usage line: real prompt/completion tokens plus Groq prompt-cache
/// hits, so users can see whether the cache (50% cheaper, TPM-exempt) is working.
pub fn print_usage_line(usage: &crate::client::groq::Usage, model: &str) {
    let prompt = usage.prompt_tokens.unwrap_or(0);
    let completion = usage.completion_tokens.unwrap_or(0);
    if prompt == 0 && completion == 0 {
        return;
    }
    let cached = usage.cached_tokens();
    let cache_part = if cached > 0 {
        format!(" ({} cached)", format_tokens_short(cached))
    } else {
        String::new()
    };
    println!(
        "{}",
        style(format!(
            "tokens: in {}{cache_part} · out {} · {model}",
            format_tokens_short(prompt),
            format_tokens_short(completion)
        ))
        .dim()
    );
}

fn format_rust_lsp_indicator(status: &RustLspStatus) -> String {
    match status.state {
        RustLspState::Off => "○ rust-lsp off".to_string(),
        RustLspState::Connecting => "◌ rust-lsp connecting".to_string(),
        RustLspState::Connected => "● rust-lsp connected".to_string(),
        RustLspState::Error => {
            let message = status.message.as_deref().unwrap_or("error");
            format!("× rust-lsp {message}")
        }
    }
}

/// Display the Vybrid ASCII banner
pub fn display_banner() {
    let banner = r#"
██╗   ██╗██╗   ██╗██████╗ ██████╗ ██╗██████╗ 
██║   ██║╚██╗ ██╔╝██╔══██╗██╔══██╗██║██╔══██╗
██║   ██║ ╚████╔╝ ██████╔╝██████╔╝██║██║  ██║
╚██╗ ██╔╝  ╚██╔╝  ██╔══██╗██╔══██╗██║██║  ██║
 ╚████╔╝    ██║   ██████╔╝██║  ██║██║██████╔╝
  ╚═══╝     ╚═╝   ╚═════╝ ╚═╝  ╚═╝╚═╝╚═════╝ 
"#;

    println!("{}", style(banner).magenta());
    println!(
        "{}",
        style("AI Coding Assistant from the Trenches built in Rust").dim()
    );
    println!(
        "{}",
        style(format!("version {}", env!("CARGO_PKG_VERSION"))).dim()
    );
    println!("{}", style("─".repeat(50)).dim());
}

/// Display mode selection header
pub fn display_mode_header() {
    println!("\n{}", style("Agent Mode Active").green().bold());
    println!("Commands: 'exit' to quit, '!' for shell mode, '!<cmd>' for single command");
}

/// Display current working directory
pub fn display_cwd() {
    if let Ok(cwd) = std::env::current_dir() {
        println!(
            "Current directory: {}\n",
            style(crate::project_context::format_path_for_display(&cwd)).cyan()
        );
    }
}

/// Print an error message
pub fn print_error(msg: &str) {
    eprintln!("{}: {}", style("Error").red().bold(), msg);
}

/// Print a success message
pub fn print_success(msg: &str) {
    println!("{}: {}", style("OK").green(), msg);
}

/// Print an info message
pub fn print_info(msg: &str) {
    println!("{}", style(msg).dim());
}

/// Print tool execution header
pub fn print_tool_execution(count: usize) {
    println!(
        "\n{}",
        style(format!("Executing {} tool(s)...", count)).yellow()
    );
}

/// Print individual tool call
pub fn print_tool_call(name: &str) {
    println!("  {} {}", style("→").dim(), name);
}

/// Print tool result
pub fn print_tool_result(name: &str, success: bool) {
    if success {
        println!("  {} {} {}", style("✓").green(), name, style("done").dim());
    } else {
        println!("  {} {} {}", style("✗").red(), name, style("failed").dim());
    }
}

/// Clear terminal screen
pub fn clear_screen() {
    let term = Term::stdout();
    let _ = term.clear_screen();
}
