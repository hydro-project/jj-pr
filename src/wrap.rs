//! Word wrapping for graph row messages.
//!
//! The graph renderer (`sapling-renderdag`) prefixes every line of a row's
//! message with the graph gutter, but performs no wrapping of its own.
//! Without help, a long PR title is emitted as a single long line, and the
//! terminal's soft wrap places the continuation at column 0 — outside the
//! gutter, visually breaking the graph. This module wraps messages to the
//! available width *before* they are handed to the renderer, so continuation
//! lines get the gutter prefix too.
//!
//! Messages contain ANSI SGR sequences (colors) and OSC 8 hyperlinks, so
//! wrapping must measure display width, not char count. `textwrap` handles
//! this: it skips ANSI CSI and OSC escape sequences when measuring and
//! segmenting words, and measures wide (e.g. CJK) characters with
//! `unicode-width`.

/// Wrap each line of a (possibly multi-line) graph row message to `width`
/// display columns.
pub fn wrap_message(message: &str, width: usize) -> String {
    let options = textwrap::Options::new(width)
        // Match terminal soft-wrap behavior: break a word that is longer
        // than the line rather than letting it overflow.
        .word_splitter(textwrap::WordSplitter::NoHyphenation)
        .break_words(true);
    message
        .lines()
        .flat_map(|line| textwrap::wrap(line, &options))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Width of the current terminal, or `None` when stdout is not a terminal
/// (so piped/redirected output is never wrapped).
#[cfg(unix)]
pub fn terminal_width() -> Option<usize> {
    use std::io::IsTerminal;
    let stdout = std::io::stdout();
    if !stdout.is_terminal() {
        return None;
    }
    let winsize = rustix::termios::tcgetwinsize(&stdout).ok()?;
    (winsize.ws_col > 0).then_some(usize::from(winsize.ws_col))
}

/// Terminal width detection is not implemented on non-Unix platforms;
/// output is left unwrapped there.
#[cfg(not(unix))]
pub fn terminal_width() -> Option<usize> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_wraps_at_spaces() {
        assert_eq!(wrap_message("one two three four", 9), "one two\nthree\nfour");
    }

    #[test]
    fn short_line_untouched() {
        assert_eq!(wrap_message("short", 80), "short");
    }

    #[test]
    fn long_word_hard_broken() {
        assert_eq!(wrap_message("abcdefghij", 4), "abcd\nefgh\nij");
    }

    #[test]
    fn sgr_sequences_are_zero_width() {
        // "bold text" is 9 columns; the SGR codes must not count.
        assert_eq!(wrap_message("\x1b[1mbold\x1b[0m text", 9), "\x1b[1mbold\x1b[0m text");
        assert_eq!(wrap_message("\x1b[1mbold\x1b[0m text", 8), "\x1b[1mbold\x1b[0m\ntext");
    }

    #[test]
    fn osc8_hyperlinks_are_zero_width() {
        // "PR #1 tail" is 10 columns; the OSC 8 link wrapper must not count.
        let line = "\x1b]8;;https://example.com\x07PR #1\x1b]8;;\x07 tail";
        assert_eq!(wrap_message(line, 10), line);
    }

    #[test]
    fn wide_chars_measured_by_display_width() {
        // Each CJK char is 2 columns wide.
        assert_eq!(wrap_message("漢字 漢字", 4), "漢字\n漢字");
    }

    #[test]
    fn unspaced_cjk_breaks_between_ideographs() {
        // Unspaced CJK text has break opportunities between characters
        // (UAX #14, via the unicode-linebreak feature).
        assert_eq!(wrap_message("長い日本語の題名", 6), "長い日\n本語の\n題名");
    }

    #[test]
    fn no_break_before_cjk_closing_punctuation() {
        // A line must not start with a full stop (kinsoku shori): the break
        // moves earlier so 。 stays attached to 名.
        assert_eq!(wrap_message("題名。", 4), "題\n名。");
    }

    #[test]
    fn multi_line_message() {
        assert_eq!(wrap_message("first line\nsecond line", 6), "first\nline\nsecond\nline");
    }
}
