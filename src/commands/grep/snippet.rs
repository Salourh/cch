//! Match-centered snippet extraction — UTF-8 safe, optional ANSI coloring.

use crate::term::{paint, BOLD_RED};

pub(super) const SNIPPET_WIDTH: usize = 140;

pub(super) fn snippet(text: &str, pos: usize, needle_len: usize) -> String {
    build_snippet(text, pos, needle_len, |m| paint(BOLD_RED, m))
}

/// Same layout as [`snippet`], but without ANSI color — used for `--json`
/// output where the match is a data field, not a display string.
pub(super) fn snippet_plain(text: &str, pos: usize, needle_len: usize) -> String {
    build_snippet(text, pos, needle_len, str::to_string)
}

fn build_snippet(
    text: &str,
    pos: usize,
    needle_len: usize,
    highlight: impl Fn(&str) -> String,
) -> String {
    let radius = SNIPPET_WIDTH.saturating_sub(needle_len) / 2;
    let start = ceil_boundary(text, pos.saturating_sub(radius));
    let end = floor_boundary(text, (pos + needle_len + radius).min(text.len()));
    let matched = &text[pos..pos + needle_len];
    let before = &text[start..pos];
    let after = &text[pos + needle_len..end];

    let mut out = String::new();
    if start > 0 {
        out.push('…');
    }
    out.push_str(&clean(before));
    out.push_str(&highlight(matched));
    out.push_str(&clean(after));
    if end < text.len() {
        out.push('…');
    }
    out
}

pub(super) fn clean(s: &str) -> String {
    s.replace(['\n', '\r', '\t'], " ")
}

fn ceil_boundary(s: &str, mut i: usize) -> usize {
    while i < s.len() && !s.is_char_boundary(i) {
        i += 1;
    }
    i
}

fn floor_boundary(s: &str, mut i: usize) -> usize {
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snippet_highlights_and_clips() {
        std::env::set_var("NO_COLOR", "1");
        let text = "a".repeat(300) + "NEEDLE" + &"b".repeat(300);
        let pos = text.find("NEEDLE").unwrap();
        let out = snippet(&text, pos, "NEEDLE".len());
        assert!(out.starts_with('…'));
        assert!(out.ends_with('…'));
        assert!(out.contains("NEEDLE"));
        assert!(out.chars().count() < 300);
    }

    #[test]
    fn snippet_plain_has_no_ansi_even_with_color_on() {
        std::env::remove_var("NO_COLOR");
        let text = "prefix NEEDLE suffix";
        let pos = text.find("NEEDLE").unwrap();
        let out = snippet_plain(text, pos, "NEEDLE".len());
        assert!(
            !out.contains('\x1b'),
            "unexpected ANSI in plain snippet: {out:?}"
        );
        assert!(out.contains("NEEDLE"));
    }

    #[test]
    fn snippet_narrow_text_has_no_ellipses() {
        std::env::set_var("NO_COLOR", "1");
        let text = "short NEEDLE here";
        let pos = text.find("NEEDLE").unwrap();
        let out = snippet(text, pos, "NEEDLE".len());
        assert!(!out.starts_with('…'));
        assert!(!out.ends_with('…'));
        assert!(out.contains("NEEDLE"));
    }

    #[test]
    fn snippet_cleans_newlines_and_tabs() {
        std::env::set_var("NO_COLOR", "1");
        let text = "pre\nNEEDLE\tpost";
        let pos = text.find("NEEDLE").unwrap();
        let out = snippet(text, pos, "NEEDLE".len());
        assert!(!out.contains('\n'));
        assert!(!out.contains('\t'));
    }
}
