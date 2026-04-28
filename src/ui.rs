use std::io::{BufRead, Write};

/// Prompt the user for confirmation. If `auto_yes` is true, prints the
/// message and proceeds without waiting. Returns true if confirmed.
pub fn confirm(message: &str, auto_yes: bool) -> bool {
    if auto_yes {
        eprintln!("{message}");
        return true;
    }
    eprint!("{message} [y/N] ");
    std::io::stderr().flush().ok();
    let mut line = String::new();
    let Ok(_) = std::io::stdin().lock().read_line(&mut line) else {
        return false;
    };
    matches!(line.trim(), "y" | "Y" | "yes" | "Yes")
}
