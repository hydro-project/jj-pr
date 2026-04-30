use std::io::{BufRead, Write};

/// Prompt the user for confirmation. Default is yes (capital Y).
/// If `auto_yes` is true, prints the message and proceeds without waiting.
pub fn confirm(message: &str, auto_yes: bool) -> bool {
    if auto_yes {
        eprintln!("{message}");
        return true;
    }
    eprint!("{message} [Y/n] ");
    std::io::stderr().flush().ok();
    let mut line = String::new();
    let Ok(_) = std::io::stdin().lock().read_line(&mut line) else {
        return true; // default yes
    };
    let lower = line.trim().to_ascii_lowercase();
    // Default yes: empty input or explicit yes.
    lower.is_empty() || matches!(lower.as_str(), "y" | "yes")
}
