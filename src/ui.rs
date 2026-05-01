use std::io::{BufRead, IsTerminal, Write};

/// Prompt the user for confirmation. Default is yes (capital Y).
/// If `auto_yes` is true, prints the message and proceeds without waiting.
/// Returns false if stdin is not a terminal (non-interactive context).
pub fn confirm(message: &str, auto_yes: bool) -> bool {
    if auto_yes {
        eprintln!("{message}");
        return true;
    }
    if !std::io::stdin().is_terminal() {
        eprintln!("{message} (non-interactive, use -y to skip prompts)");
        return false;
    }
    eprint!("{message} [Y/n] ");
    std::io::stderr().flush().ok();
    let mut line = String::new();
    let Ok(n) = std::io::stdin().lock().read_line(&mut line) else {
        return false;
    };
    if n == 0 {
        return false; // EOF
    }
    let lower = line.trim().to_ascii_lowercase();
    // Default yes: empty input or explicit yes.
    lower.is_empty() || matches!(lower.as_str(), "y" | "yes")
}
