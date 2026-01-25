#![allow(dead_code)]

use console::{style, Term};

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
    println!("{}", style("AI Coding Assistant - GLM-4.7").dim());
    println!("{}", style("─".repeat(50)).dim());
}

/// Display mode selection header
pub fn display_mode_header(mode: &str) {
    match mode {
        "agent" => {
            println!("\n{}", style("Agent Mode Active").green().bold());
            println!("Commands: 'exit' to quit, '!' for shell mode, '!<cmd>' for single command");
        }
        "daemon" => {
            println!("\n{}", style("Daemon Mode Active").yellow().bold());
            println!("Background service running. Press Ctrl+C to stop.");
        }
        _ => {}
    }
}

/// Display current working directory
pub fn display_cwd() {
    if let Ok(cwd) = std::env::current_dir() {
        let display_path = if let Some(home) = dirs::home_dir() {
            cwd.strip_prefix(&home)
                .map(|p| format!("~/{}", p.display()))
                .unwrap_or_else(|_| cwd.display().to_string())
        } else {
            cwd.display().to_string()
        };
        println!("Current directory: {}\n", style(display_path).cyan());
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
