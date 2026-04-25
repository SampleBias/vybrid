#![allow(dead_code)]

use anyhow::Result;
use console::style;
use rustyline::completion::{Completer, FilenameCompleter, Pair};
use rustyline::error::ReadlineError;
use rustyline::highlight::Highlighter;
use rustyline::hint::Hinter;
use rustyline::history::DefaultHistory;
use rustyline::validate::Validator;
use rustyline::{Context, Editor, Helper};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

const INTERACTIVE_COMMAND_TIMEOUT_SECS: u64 = 300;

struct ShellLineHelper {
    completer: FilenameCompleter,
}

impl ShellLineHelper {
    fn new() -> Self {
        Self {
            completer: FilenameCompleter::new(),
        }
    }
}

impl Helper for ShellLineHelper {}
impl Validator for ShellLineHelper {}
impl Highlighter for ShellLineHelper {}

impl Hinter for ShellLineHelper {
    type Hint = String;
}

impl Completer for ShellLineHelper {
    type Candidate = Pair;

    fn complete(
        &self,
        line: &str,
        pos: usize,
        ctx: &Context<'_>,
    ) -> rustyline::Result<(usize, Vec<Pair>)> {
        self.completer.complete(line, pos, ctx)
    }
}

/// Persistent shell session that maintains state
pub struct PersistentShell {
    process: Child,
    shell_cwd: String,
}

impl PersistentShell {
    /// Start a new persistent shell
    pub fn new() -> Result<Self> {
        let initial_cwd = std::env::current_dir()?.to_string_lossy().to_string();

        let process = Command::new("bash")
            .arg("--norc")
            .arg("--noprofile")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .current_dir(&initial_cwd)
            .spawn()?;

        Ok(Self {
            process,
            shell_cwd: initial_cwd,
        })
    }

    /// Execute a command and return the output
    pub fn execute(&mut self, command: &str) -> Result<String> {
        let stdin = self
            .process
            .stdin
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("Failed to get stdin"))?;

        // Create a unique marker for command completion
        let marker = format!("__VYBRID_END_{}__", std::process::id());

        // Send command with marker and pwd for directory tracking
        let full_command = if command.trim().starts_with("cd ") {
            format!("{}; echo '{}'; pwd\n", command, marker)
        } else {
            format!("{}; echo '{}'\n", command, marker)
        };

        stdin.write_all(full_command.as_bytes())?;
        stdin.flush()?;

        // Read output until we see the marker
        let stdout = self
            .process
            .stdout
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("Failed to get stdout"))?;

        let mut reader = BufReader::new(stdout);
        let mut output = String::new();
        let mut line = String::new();

        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) => break, // EOF
                Ok(_) => {
                    if line.contains(&marker) {
                        // If this was a cd command, the next line is the new directory
                        if command.trim().starts_with("cd ") {
                            line.clear();
                            if reader.read_line(&mut line).is_ok() && !line.is_empty() {
                                self.shell_cwd = line.trim().to_string();
                            }
                        }
                        break;
                    }
                    output.push_str(&line);
                }
                Err(e) => return Err(anyhow::anyhow!("Read error: {}", e)),
            }
        }

        Ok(output)
    }

    /// Get current working directory
    pub fn cwd(&self) -> &str {
        &self.shell_cwd
    }

    /// Sync Vybrid's directory to match shell's
    pub fn sync_directory(&self) -> Result<()> {
        std::env::set_current_dir(&self.shell_cwd)?;
        Ok(())
    }
}

impl Drop for PersistentShell {
    fn drop(&mut self) {
        // Try to gracefully terminate
        if let Some(stdin) = self.process.stdin.as_mut() {
            let _ = stdin.write_all(b"exit\n");
        }

        // Wait briefly, then kill if needed
        thread::sleep(Duration::from_millis(100));
        let _ = self.process.kill();
    }
}

/// Enter interactive persistent shell mode
pub fn enter_shell_mode() -> Result<()> {
    println!("{}", style("Entering Persistent Shell Mode").cyan().bold());
    println!("{}", style("Directory changes and state persist!").dim());
    println!(
        "{}",
        style("Type 'exit' or press Enter on empty line to return to Vybrid").dim()
    );
    println!();

    // Display current directory
    let cwd = std::env::current_dir()?;
    println!("Current directory: {}", style(cwd.display()).yellow());
    println!();

    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();

    // Handle Ctrl+C gracefully
    ctrlc::set_handler(move || {
        r.store(false, Ordering::SeqCst);
    })
    .ok();

    let mut editor = Editor::<ShellLineHelper, DefaultHistory>::new()?;
    editor.set_helper(Some(ShellLineHelper::new()));

    // Simple shell loop using standard process execution
    'shell: while running.load(Ordering::SeqCst) {
        // Get current directory for prompt
        let cwd = std::env::current_dir()
            .map(|p| {
                if let Some(home) = dirs::home_dir() {
                    p.strip_prefix(&home)
                        .map(|rel| format!("~/{}", rel.display()))
                        .unwrap_or_else(|_| p.display().to_string())
                } else {
                    p.display().to_string()
                }
            })
            .unwrap_or_else(|_| "?".to_string());

        // Get base directory name
        let dir_name = std::path::Path::new(&cwd)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or(cwd.clone());

        // Read input
        let prompt = format!("{} {} ", style("shell").green(), style(dir_name).cyan());
        let input = match editor.readline(&prompt) {
            Ok(input) => input,
            Err(ReadlineError::Interrupted) | Err(ReadlineError::Eof) => break,
            Err(e) => {
                println!("{}: {}", style("Error").red(), e);
                break;
            }
        };

        let input = input.trim();

        // Handle exit
        if input.is_empty() || input == "exit" || input == "quit" {
            break;
        }

        // Handle cd specially to update Vybrid's cwd
        if let Some(path) = input.strip_prefix("cd ") {
            let path = path.trim();
            let new_path = if let Some(stripped) = path.strip_prefix("~/") {
                if let Some(home) = dirs::home_dir() {
                    home.join(stripped)
                } else {
                    std::path::PathBuf::from(path)
                }
            } else if path == "~" {
                dirs::home_dir().unwrap_or_else(|| std::path::PathBuf::from("/"))
            } else {
                std::path::PathBuf::from(path)
            };

            match std::env::set_current_dir(&new_path) {
                Ok(_) => {
                    println!(
                        "{}",
                        style(format!("Changed to: {}", new_path.display())).dim()
                    );
                }
                Err(e) => {
                    println!("{}: {}", style("Error").red(), e);
                }
            }
            continue;
        }

        // Handle pwd
        if input == "pwd" {
            match std::env::current_dir() {
                Ok(cwd) => println!("{}", cwd.display()),
                Err(e) => println!("{}: {}", style("Error").red(), e),
            }
            continue;
        }

        // Handle clear
        if input == "clear" {
            print!("\x1B[2J\x1B[1;1H");
            std::io::stdout().flush()?;
            continue;
        }

        // Execute other commands with a wall-clock timeout so shell mode cannot hang forever.
        let mut child = match Command::new("bash")
            .arg("-c")
            .arg(input)
            .current_dir(std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("/")))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(child) => child,
            Err(e) => {
                println!("{}: {}", style("Error").red(), e);
                continue;
            }
        };

        let started = Instant::now();
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None)
                    if started.elapsed()
                        > Duration::from_secs(INTERACTIVE_COMMAND_TIMEOUT_SECS) =>
                {
                    let _ = child.kill();
                    let _ = child.wait();
                    println!(
                        "{}: command timed out after {}s",
                        style("Error").red(),
                        INTERACTIVE_COMMAND_TIMEOUT_SECS
                    );
                    continue 'shell;
                }
                Ok(None) => thread::sleep(Duration::from_millis(50)),
                Err(e) => {
                    println!("{}: {}", style("Error").red(), e);
                    continue 'shell;
                }
            }
        }

        match child.wait_with_output() {
            Ok(output) => {
                if !output.stdout.is_empty() {
                    print!("{}", String::from_utf8_lossy(&output.stdout));
                }
                if !output.stderr.is_empty() {
                    eprint!("{}", String::from_utf8_lossy(&output.stderr));
                }
            }
            Err(e) => {
                println!("{}: {}", style("Error").red(), e);
            }
        }
    }

    println!();
    println!("{}", style("Exited shell mode").dim());

    // Display synced directory
    if let Ok(cwd) = std::env::current_dir() {
        println!("Vybrid directory: {}", style(cwd.display()).cyan());
    }

    Ok(())
}
