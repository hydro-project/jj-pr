use anstyle::{AnsiColor, Effects, Style};

use crate::gh::PrState;

// --- Glyphs ---
pub const GLYPH_IMMUTABLE: &str = "◆";
pub const GLYPH_MUTABLE: &str = "○";
pub const GLYPH_CONFLICTED: &str = "✗";
pub const GLYPH_WARNING: &str = "⚠";

// --- Labels ---
pub const LABEL_TRUNK: &str = "trunk()";
pub const LABEL_ROOT: &str = "(elided revisions)";

// --- Styles ---
const BOLD: Style = Style::new().effects(Effects::BOLD);
const DIM: Style = Style::new().effects(Effects::DIMMED);
const IMMUTABLE: Style = Style::new()
    .fg_color(Some(anstyle::Color::Ansi(AnsiColor::Cyan)))
    .effects(Effects::BOLD);
const CURRENT: Style = Style::new()
    .fg_color(Some(anstyle::Color::Ansi(AnsiColor::Green)))
    .effects(Effects::BOLD);
const PR_NUM: Style = Style::new()
    .fg_color(Some(anstyle::Color::Ansi(AnsiColor::Cyan)))
    .effects(Effects::BOLD);
const BOOKMARK: Style = Style::new()
    .fg_color(Some(anstyle::Color::Ansi(AnsiColor::Magenta)))
    .effects(Effects::BOLD);
const CHANGE_ID: Style = Style::new()
    .fg_color(Some(anstyle::Color::Ansi(AnsiColor::Magenta)))
    .effects(Effects::BOLD);
const COMMIT_ID: Style = Style::new().fg_color(Some(anstyle::Color::Ansi(AnsiColor::Blue)));
const DESCRIPTION: Style = Style::new();
const EMPTY: Style = Style::new().fg_color(Some(anstyle::Color::Ansi(AnsiColor::Green)));

const DRAFT: Style = Style::new().fg_color(Some(anstyle::Color::Ansi(AnsiColor::Yellow)));
const READY: Style = Style::new().fg_color(Some(anstyle::Color::Ansi(AnsiColor::Green)));
const MERGED: Style = Style::new().effects(Effects::DIMMED);
const CLOSED: Style = Style::new().fg_color(Some(anstyle::Color::Ansi(AnsiColor::Red)));
const TRUNK_LABEL: Style = Style::new()
    .fg_color(Some(anstyle::Color::Ansi(AnsiColor::Green)))
    .effects(Effects::BOLD);

const WARN: Style = Style::new().fg_color(Some(anstyle::Color::Ansi(AnsiColor::Yellow)));

/// Override for forcing color on (used in tests).
static FORCE_COLOR: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Force color output on, regardless of terminal detection.
#[cfg(test)]
pub fn set_force_color(force: bool) {
    FORCE_COLOR.store(force, std::sync::atomic::Ordering::Relaxed);
}

fn use_color() -> bool {
    FORCE_COLOR.load(std::sync::atomic::Ordering::Relaxed)
        || anstream::AutoStream::choice(&std::io::stderr()) != anstream::ColorChoice::Never
}

fn osc8(url: &str, text: &str) -> String {
    if use_color() {
        format!("\x1b]8;;{url}\x07{text}\x1b]8;;\x07")
    } else {
        text.to_owned()
    }
}

fn styled(style: Style, text: &str) -> String {
    if use_color() {
        format!("{style}{text}{style:#}")
    } else {
        text.to_owned()
    }
}

/// Format a PR number as a clickable hyperlink (if URL provided) with bold cyan styling.
pub fn pr_num(number: crate::gh::PrNum, url: Option<&str>) -> String {
    let text = format!("PR #{}", number.get());
    let linked = match url {
        Some(u) => osc8(u, &text),
        None => text,
    };
    styled(PR_NUM, &linked)
}

pub fn bookmark(name: &str) -> String {
    styled(BOOKMARK, name)
}

pub fn status(state: PrState, is_draft: bool) -> String {
    match (state, is_draft) {
        (PrState::Closed, _) => styled(CLOSED, "Closed"),
        (PrState::Open, true) => styled(DRAFT, "Draft"),
        (PrState::Open, false) => styled(READY, "Ready"),
        (PrState::Merged, _) => styled(MERGED, "Merged"),
    }
}

pub fn change_id(id: &str) -> String {
    // Show first 12 chars in bold magenta (like jj's unique prefix style).
    let short = &id[..12.min(id.len())];
    styled(CHANGE_ID, short)
}

pub fn commit_id_short(id: &str) -> String {
    let short = &id[..12.min(id.len())];
    styled(COMMIT_ID, short)
}

pub fn description_first_line(desc: &str) -> String {
    let line = desc.lines().next().unwrap_or("");
    if line.is_empty() {
        styled(EMPTY, "(no description set)")
    } else {
        styled(DESCRIPTION, line)
    }
}

pub fn empty_marker() -> String {
    styled(EMPTY, "(empty)")
}

pub fn bookmark_label(name: &str) -> String {
    styled(BOOKMARK, name)
}

#[expect(dead_code, reason = "available for UI")]
pub fn bold(text: &str) -> String {
    styled(BOLD, text)
}

pub fn glyph_immutable() -> String {
    styled(IMMUTABLE, GLYPH_IMMUTABLE)
}

pub fn glyph_elided() -> String {
    styled(DIM, "~")
}

pub fn glyph_conflicted() -> String {
    styled(CLOSED, GLYPH_CONFLICTED)
}

pub fn glyph_current() -> String {
    styled(CURRENT, "@")
}

pub fn dim(text: &str) -> String {
    styled(DIM, text)
}

pub fn warn(text: &str) -> String {
    styled(WARN, text)
}

pub fn trunk() -> String {
    styled(TRUNK_LABEL, LABEL_TRUNK)
}

pub fn root() -> String {
    styled(DIM, LABEL_ROOT)
}

pub fn ci_status(status: crate::gh::CheckStatus) -> String {
    match status {
        crate::gh::CheckStatus::Pass => styled(READY, "✓CI"),
        crate::gh::CheckStatus::Fail => styled(CLOSED, "✗CI"),
        crate::gh::CheckStatus::Pending => styled(DRAFT, "●CI"),
    }
}

pub fn review_status(decision: Option<crate::gh::ReviewDecision>) -> String {
    match decision {
        Some(crate::gh::ReviewDecision::Approved) => styled(READY, "✓Review"),
        Some(crate::gh::ReviewDecision::ChangesRequested) => styled(CLOSED, "✗Review"),
        Some(crate::gh::ReviewDecision::ReviewRequired) | None => styled(DRAFT, "●Review"),
    }
}
