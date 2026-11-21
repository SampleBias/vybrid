use console::{style, Term};
use rustyline::completion::Completer;
use rustyline::error::ReadlineError;
use rustyline::highlight::Highlighter;
use rustyline::hint::{Hint, Hinter};
use rustyline::history::MemHistory;
use rustyline::validate::Validator;
use rustyline::{Context, Editor, Helper};
use std::borrow::Cow;
use std::cell::Cell;
use std::io::{self, Write};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

const LABEL: &str = "You>";

fn rule() -> String {
    let columns = Term::stdout().size().1;
    "─".repeat(usize::from(columns.saturating_sub(1)))
}

#[derive(Default)]
struct Frame {
    columns: Cell<Option<u16>>,
    resized: Cell<bool>,
}

// Position in the draft, including the "You> " prefix. A column equal to
// the terminal width means the last glyph is sitting on the right margin.
fn input_position(text: &str, columns: usize) -> (usize, usize) {
    let mut row = LABEL.len() / columns;
    let mut column = LABEL.len() % columns + 1;
    for glyph in text.graphemes(true) {
        if glyph == "\n" {
            row += 1;
            column = 0;
        } else {
            let cells = if glyph == "\t" {
                8 - column % 8
            } else {
                glyph.width()
            };
            if column + cells > columns {
                row += 1;
                column = 0;
            }
            column += cells;
        }
    }
    (row, column)
}

impl Helper for Frame {}
impl Validator for Frame {}
impl Completer for Frame {
    type Candidate = String;
}

// The editor lays out the footer with the input, including when a paste wraps.
// It is display-only, so accepting a hint can never insert the rule into a task.
struct Footer(String);

impl Hint for Footer {
    fn display(&self) -> &str {
        &self.0
    }

    fn completion(&self) -> Option<&str> {
        None
    }
}

impl Hinter for Frame {
    type Hint = Footer;

    fn hint(&self, line: &str, pos: usize, _: &Context<'_>) -> Option<Footer> {
        let columns = Term::stdout().size().1.max(1);
        if let Some(old) = self
            .columns
            .replace(Some(columns))
            .filter(|old| *old != columns)
        {
            let new_width = usize::from(columns);
            let old_width = usize::from(old);
            // The terminal has reflowed the old rule and draft. Rustyline will
            // clear relative to its pre-resize cursor row on this same refresh.
            let draft = &line[..pos];
            let cursor_row = |width| {
                let (row, column) = input_position(draft, width);
                row + usize::from(column == width)
            };
            let reflowed = old_width.saturating_sub(2) / new_width + cursor_row(new_width);
            let previous = cursor_row(old_width);
            let term = Term::stdout();
            if reflowed > previous {
                let _ = term.move_cursor_up(reflowed - previous);
            } else if previous > reflowed {
                let _ = term.move_cursor_down(previous - reflowed);
            }
            self.resized.set(true);
        }
        // Materialize a pending soft wrap only at the right margin.
        let width = usize::from(columns);
        let padding = if input_position(line, width).1 == width {
            " "
        } else {
            ""
        };
        Some(Footer(format!("{padding}\n{}", rule())))
    }
}

impl Highlighter for Frame {
    fn highlight_prompt<'b, 's: 'b, 'p: 'b>(
        &'s self,
        prompt: &'p str,
        default: bool,
    ) -> Cow<'b, str> {
        if default {
            // The raw prompt reserves one whole row for the width-dependent rule.
            let clear_below = if self.resized.replace(false) {
                "\x1b[J"
            } else {
                ""
            };
            format!(
                "{clear_below}{}\n{} ",
                style(rule()).dim(),
                style(LABEL).magenta().bold()
            )
            .into()
        } else {
            prompt.into()
        }
    }

    fn highlight_hint<'h>(&self, hint: &'h str) -> Cow<'h, str> {
        style(hint).dim().to_string().into()
    }
}

pub struct ChatPrompt {
    editor: Option<Editor<Frame, MemHistory>>,
}

impl ChatPrompt {
    pub fn new() -> io::Result<Self> {
        let editor = if super::interactive_terminal() {
            // Always draw the frame. console::style handles color preferences separately.
            let config = rustyline::Config::builder()
                .color_mode(rustyline::ColorMode::Forced)
                .build();
            let mut editor =
                Editor::with_history(config, MemHistory::new()).map_err(io::Error::other)?;
            editor.set_helper(Some(Frame::default()));
            Some(editor)
        } else {
            None
        };
        Ok(Self { editor })
    }

    pub fn read_line(&mut self) -> io::Result<Option<String>> {
        if let Some(editor) = &mut self.editor {
            if let Some(frame) = editor.helper() {
                frame.columns.set(None);
            }
            match editor.readline("\nYou> ") {
                Ok(line) => {
                    // Rustyline removes hints on submit. Commit the lower rule to scrollback.
                    println!("{}", style(rule()).dim());
                    Ok(Some(line))
                }
                Err(ReadlineError::Eof | ReadlineError::Interrupted) => Ok(None),
                Err(error) => Err(io::Error::other(error)),
            }
        } else {
            print!("{} ", style(LABEL).magenta().bold());
            io::stdout().flush()?;
            let mut line = String::new();
            let bytes = io::stdin().read_line(&mut line)?;
            Ok((bytes > 0).then_some(line))
        }
    }
}
