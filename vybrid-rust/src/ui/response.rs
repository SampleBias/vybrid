use super::TerminalWriter;
use console::{style, Term};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

/// Formatting is local to one stream attempt. Only original deltas enter the conversation.
pub struct ResponseDisplay {
    framed: bool,
    section: Section,
}

enum Section {
    Idle,
    Answer(TerminalWriter),
    PlainThinking(TerminalWriter),
    BoxedThinking(ThinkingLines),
}

fn border(text: &str) -> String {
    style(text).color256(139).to_string()
}

impl Default for ResponseDisplay {
    fn default() -> Self {
        Self {
            framed: super::interactive_terminal(),
            section: Section::Idle,
        }
    }
}

impl ResponseDisplay {
    pub fn thinking(&mut self, text: &str) -> String {
        let mut out = String::new();
        if matches!(self.section, Section::Idle | Section::Answer(_)) {
            if text.trim().is_empty() {
                return out;
            }
            out.push_str(&self.finish());
            if self.framed {
                out.push_str(&border("╭─ Thinking"));
                self.section = Section::BoxedThinking(ThinkingLines::default());
            } else {
                out.push_str("Thinking:");
                self.section = Section::PlainThinking(TerminalWriter::new());
            }
            out.push_str("\r\n");
        }
        match &mut self.section {
            Section::BoxedThinking(lines) => {
                lines.push(text, &mut out, Term::stdout().size().1.into())
            }
            Section::PlainThinking(lines) => out.push_str(&lines.push(text)),
            _ => unreachable!(),
        }
        out
    }

    pub fn answer(&mut self, text: &str) -> String {
        let mut out = String::new();
        if !matches!(self.section, Section::Answer(_)) {
            out.push_str(&self.finish());
            out.push_str(&format!("{} ", style("Assistant>").cyan().bold()));
            self.section = Section::Answer(TerminalWriter::new());
        }
        if let Section::Answer(lines) = &mut self.section {
            out.push_str(&lines.push(text));
        }
        out
    }

    /// Close before switching to another speaker, tool output, or retry diagnostics.
    pub fn finish(&mut self) -> String {
        match std::mem::replace(&mut self.section, Section::Idle) {
            Section::Idle => String::new(),
            Section::Answer(mut lines) | Section::PlainThinking(mut lines) => {
                format!("{}\r\n", lines.finish())
            }
            Section::BoxedThinking(mut lines) => {
                let mut out = String::new();
                let last = std::mem::take(&mut lines.pending);
                if !last.is_empty() {
                    lines.draw(&last, &mut out, Term::stdout().size().1.into());
                }
                if lines.column > 0 {
                    out.push_str("\r\n");
                }
                out.push_str(&border("╰─"));
                out.push_str("\r\n\r\n");
                out
            }
        }
    }
}

#[derive(Default)]
struct ThinkingLines {
    column: usize,
    pending: String,
}

impl ThinkingLines {
    fn push(&mut self, text: &str, out: &mut String, width: usize) {
        // A chunk can end inside CRLF, a combining sequence or a joined emoji.
        let mut text_buffer = std::mem::take(&mut self.pending);
        text_buffer.push_str(text);
        let mut glyphs = text_buffer.graphemes(true);
        self.pending
            .push_str(glyphs.next_back().unwrap_or_default());
        for glyph in glyphs {
            self.draw(glyph, out, width);
        }
    }

    fn draw(&mut self, glyph: &str, out: &mut String, width: usize) {
        if glyph == "\t" {
            for _ in 0..8 - self.column.max(2) % 8 {
                self.draw(" ", out, width);
            }
            return;
        }
        let newline = matches!(
            glyph,
            "\r\n" | "\r" | "\n" | "\x0b" | "\x0c" | "\u{85}" | "\u{2028}" | "\u{2029}"
        );
        let cells = glyph.width();
        if !newline && self.column > 2 && self.column + cells > width {
            out.push_str("\r\n");
            self.column = 0;
        }
        if self.column == 0 {
            out.push_str(&border("│"));
            out.push(' ');
            self.column = 2;
        }
        if newline {
            out.push_str("\r\n");
            self.column = 0;
        } else {
            out.push_str(glyph);
            self.column += cells;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_split_cluster_at_the_margin_wraps_with_its_gutter() {
        let mut lines = ThinkingLines::default();
        let mut out = String::new();
        for chunk in ["abcd👩", "\u{200d}", "💻e", "\u{301}\r", "\n", "z\n!"] {
            lines.push(chunk, &mut out, 7);
        }
        assert_eq!(
            console::strip_ansi_codes(&out),
            "│ abcd\r\n│ 👩‍💻e\u{301}\r\n│ z\r\n"
        );
        assert_eq!(lines.pending, "!");
    }

    #[test]
    fn thinking_closes_once_before_text_tools_and_errors() {
        let mut display = ResponseDisplay {
            framed: true,
            section: Section::Idle,
        };
        assert_eq!(display.thinking(" \n"), "");
        let mut out = display.thinking("one\r");
        out.push_str(&display.thinking("\ntwo"));
        out.push_str(&display.answer("answer"));
        out.push_str(&display.finish());
        assert_eq!(
            console::strip_ansi_codes(&out),
            "╭─ Thinking\r\n│ one\r\n│ two\r\n╰─\r\n\r\nAssistant> answer\r\n"
        );
        let mut before_tool = display.thinking("tool\n");
        before_tool.push_str(&display.finish());
        assert_eq!(
            console::strip_ansi_codes(&before_tool),
            "╭─ Thinking\r\n│ tool\r\n╰─\r\n\r\n"
        );
        assert!(display.finish().is_empty());
    }

    #[test]
    fn redirected_responses_use_plain_labels_and_text() {
        let mut display = ResponseDisplay {
            framed: false,
            section: Section::Idle,
        };
        let mut out = display.thinking("one\r");
        out.push_str(&display.thinking("\ntwo"));
        out.push_str(&display.answer("three"));
        assert_eq!(
            console::strip_ansi_codes(&out),
            "Thinking:\r\none\r\ntwo\r\nAssistant> three"
        );
    }
}
