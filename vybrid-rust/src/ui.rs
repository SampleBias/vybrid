#![allow(dead_code)]

use console::{style, Term};
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::lsp::{RustLspState, RustLspStatus};

/// Approximate `openai/gpt-oss-120b` context window (tokens). Used only for the CLI meter.
pub const CONTEXT_WINDOW_TOKENS: u32 = 131_072;

/// Crush-style activity line. Scrambled glyphs mean the model is working and has
/// not produced visible text yet. A tool run uses a plain label and elapsed time.
const SPINNER_RUNES: &[char] = &[
    '0', '1', '2', '3', '4', '5', '6', '7', '8', '9', 'a', 'b', 'c', 'd', 'e', 'f', 'A', 'B', 'C',
    'D', 'E', 'F', '~', '!', '@', '#', '$', '£', '€', '%', '^', '&', '*', '(', ')', '+', '=', '_',
];
const SPINNER_GLYPHS: usize = 10;
const SPINNER_FRAME: std::time::Duration = std::time::Duration::from_millis(50);

enum SpinnerKind {
    Thinking,
    Running(String),
    Status(String),
}

pub struct SpinnerGuard {
    stop: Arc<AtomicBool>,
    handle: Option<tokio::task::JoinHandle<()>>,
}

impl SpinnerGuard {
    /// Scrambled glyph row plus `Thinking...`, until the model prints something.
    pub fn thinking() -> Self {
        Self::spawn(SpinnerKind::Thinking)
    }

    /// `Running {tool}... 12s` while a tool executes. No scrambled glyphs.
    pub fn running(tool: impl Into<String>) -> Self {
        Self::spawn(SpinnerKind::Running(tool.into()))
    }

    /// `{label}... 2s` for short non-model waits such as Jev, compaction, or a rate limit.
    pub fn status(label: impl Into<String>) -> Self {
        Self::spawn(SpinnerKind::Status(label.into()))
    }

    fn spawn(kind: SpinnerKind) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let stop_clone = stop.clone();
        let handle = tokio::spawn(async move {
            let started = std::time::Instant::now();
            let mut tick = 0u32;
            loop {
                let frame = render_spinner_frame(&kind, tick, started.elapsed().as_secs());
                {
                    let _guard = lock_activity();
                    if stop_clone.load(Ordering::Relaxed) {
                        break;
                    }
                    eprint!("\r\x1b[2K{frame}");
                    let _ = std::io::stderr().flush();
                }
                tick = tick.wrapping_add(1);
                tokio::time::sleep(SPINNER_FRAME).await;
            }
        });
        Self {
            stop,
            handle: Some(handle),
        }
    }

    pub async fn finish(&mut self) {
        if let Some(handle) = self.handle.take() {
            self.stop.store(true, Ordering::Relaxed);
            let _ = handle.await;
            clear_activity_line();
        }
    }
}

impl Drop for SpinnerGuard {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            handle.abort();
            clear_activity_line();
        }
    }
}

/// Print a full stderr line without appending it to the spinner's current row.
pub fn eprintln_status(line: impl std::fmt::Display) {
    let _guard = lock_activity();
    eprintln!("\r\x1b[2K{line}");
}

fn activity_draw() -> &'static Mutex<()> {
    static DRAW: Mutex<()> = Mutex::new(());
    &DRAW
}

fn lock_activity() -> std::sync::MutexGuard<'static, ()> {
    activity_draw()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn clear_activity_line() {
    let _guard = lock_activity();
    eprint!("\r\x1b[2K");
    let _ = std::io::stderr().flush();
}

fn render_spinner_frame(kind: &SpinnerKind, tick: u32, elapsed_secs: u64) -> String {
    let dots = ellipsis(tick);
    match kind {
        SpinnerKind::Thinking => {
            format!("{}\x1b[2m Thinking{dots}\x1b[0m", colored_glyphs(tick))
        }
        SpinnerKind::Running(tool) => format!("Running {tool}{dots} {elapsed_secs}s"),
        SpinnerKind::Status(label) => format!("{label}{dots} {elapsed_secs}s"),
    }
}

/// Width-3 ellipsis so the elapsed-seconds column does not jump as the dots grow.
fn ellipsis(tick: u32) -> &'static str {
    match (tick / 8) % 4 {
        0 => ".  ",
        1 => ".. ",
        2 => "...",
        _ => "   ",
    }
}

fn colored_glyphs(tick: u32) -> String {
    let mut out = String::with_capacity(SPINNER_GLYPHS * 16);
    for (index, glyph) in glyph_frame(tick).into_iter().enumerate() {
        let (red, green, blue) = glyph_color(index);
        out.push_str(&format!("\x1b[38;2;{red};{green};{blue}m{glyph}"));
    }
    out.push_str("\x1b[0m");
    out
}

fn glyph_frame(tick: u32) -> [char; SPINNER_GLYPHS] {
    let mut state = 0xA5A5_u32.wrapping_add(tick.wrapping_mul(0x9E37_79B9));
    let mut glyphs = [' '; SPINNER_GLYPHS];
    for glyph in &mut glyphs {
        state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        let index = (state >> 16) as usize % SPINNER_RUNES.len();
        *glyph = SPINNER_RUNES[index];
    }
    glyphs
}

fn glyph_color(index: usize) -> (u8, u8, u8) {
    let span = SPINNER_GLYPHS.saturating_sub(1).max(1);
    let blue = (index * 255 / span) as u8;
    (255 - blue, 0, blue)
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

/// Turns model text into terminal output whose line breaks return to column 0.
///
/// A bare line feed, Unicode line separator, or next-line character moves the
/// cursor down but leaves it at the current column. That draws each following
/// line further to the right. Emitting CR LF keeps every line on the left edge,
/// including when a break is split across two stream chunks.
pub struct TerminalWriter {
    pending_cr: bool,
}

impl TerminalWriter {
    pub fn new() -> Self {
        Self { pending_cr: false }
    }

    pub fn reset(&mut self) {
        self.pending_cr = false;
    }

    pub fn push(&mut self, text: &str) -> String {
        let mut out = String::with_capacity(text.len());
        let mut chars = text.chars().peekable();
        if self.pending_cr {
            if chars.peek() == Some(&'\n') {
                chars.next();
            }
            out.push_str("\r\n");
            self.pending_cr = false;
        }
        while let Some(ch) = chars.next() {
            match ch {
                '\r' => {
                    if chars.peek() == Some(&'\n') {
                        chars.next();
                        out.push_str("\r\n");
                    } else if chars.peek().is_some() {
                        out.push_str("\r\n");
                    } else {
                        self.pending_cr = true;
                    }
                }
                '\n' | '\u{000b}' | '\u{000c}' | '\u{0085}' | '\u{2028}' | '\u{2029}' => {
                    out.push_str("\r\n");
                }
                _ => out.push(ch),
            }
        }
        out
    }

    pub fn finish(&mut self) -> &'static str {
        if self.pending_cr {
            self.pending_cr = false;
            "\r\n"
        } else {
            ""
        }
    }
}

impl Default for TerminalWriter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ellipsis, glyph_color, glyph_frame, render_spinner_frame, SpinnerKind, TerminalWriter,
        SPINNER_GLYPHS, SPINNER_RUNES,
    };

    #[test]
    fn line_breaks_return_to_column_zero() {
        let mut writer = TerminalWriter::new();
        assert_eq!(writer.push("a\nb"), "a\r\nb");
        assert_eq!(writer.push("a\r\nb"), "a\r\nb");
        assert_eq!(writer.push("a\rb"), "a\r\nb");
        assert_eq!(writer.push("a\u{2028}b"), "a\r\nb");
        assert_eq!(writer.push("a\u{2029}b"), "a\r\nb");
        assert_eq!(writer.push("a\u{0085}b"), "a\r\nb");
        assert_eq!(writer.push("\n\n"), "\r\n\r\n");
        assert_eq!(writer.finish(), "");
    }

    #[test]
    fn carriage_return_split_across_chunks_stays_one_break() {
        let mut writer = TerminalWriter::new();
        assert_eq!(writer.push("left\r"), "left");
        assert_eq!(writer.push("\nright"), "\r\nright");
        assert_eq!(writer.finish(), "");
    }

    #[test]
    fn spinner_frames_match_the_crush_loader() {
        assert_eq!(ellipsis(0), ".  ");
        assert_eq!(ellipsis(8), ".. ");
        assert_eq!(ellipsis(16), "...");
        assert_eq!(ellipsis(24), "   ");

        let glyphs = glyph_frame(0);
        assert_eq!(glyphs.len(), SPINNER_GLYPHS);
        assert!(glyphs.iter().all(|glyph| SPINNER_RUNES.contains(glyph)));
        assert_ne!(glyph_frame(0), glyph_frame(1));
        assert_eq!(glyph_color(0), (255, 0, 0));
        assert_eq!(glyph_color(SPINNER_GLYPHS - 1), (0, 0, 255));

        let thinking = render_spinner_frame(&SpinnerKind::Thinking, 16, 4);
        assert!(thinking.contains("\x1b[38;2;"));
        assert!(thinking.contains("\x1b[2m Thinking...\x1b[0m"));
        assert!(!thinking.contains("4s"));

        assert_eq!(
            render_spinner_frame(&SpinnerKind::Running("run_cargo".into()), 16, 12),
            "Running run_cargo... 12s"
        );
        assert_eq!(
            render_spinner_frame(&SpinnerKind::Status("jev".into()), 0, 2),
            "jev.   2s"
        );
    }

    #[tokio::test]
    async fn spinner_finish_returns_after_the_frame_task_stops() {
        let mut spinner = super::SpinnerGuard::running("run_cargo");
        tokio::time::sleep(std::time::Duration::from_millis(60)).await;
        spinner.finish().await;
    }
}
