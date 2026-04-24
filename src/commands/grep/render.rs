//! Presentation layer — headers, hit rows, JSON/ids/default output modes.

use crate::term::{paint, BOLD, CYAN, DIM, YELLOW};
use crate::transcript::Role;

use super::opts::{HitKind, SessionHits};
use super::snippet::clean;

// Indirections so we only have to tweak the color palette in one place.
const GREEN_ROLE: &str = crate::term::GREEN;
const MAGENTA_ROLE: &str = crate::term::MAGENTA;

pub(super) fn emit_ids(results: &[SessionHits]) {
    use std::io::Write;
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    for r in results {
        if let Some(id) = r.path.file_stem().and_then(|s| s.to_str()) {
            let _ = writeln!(out, "{id}");
        }
    }
}

pub(super) fn emit_json(results: &[SessionHits]) {
    use std::io::Write;
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    for r in results {
        let id = r.path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        let project = r
            .path
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|s| s.to_str())
            .unwrap_or("");
        for h in &r.hits {
            if h.kind != HitKind::Match {
                continue;
            }
            let obj = serde_json::json!({
                "session": id,
                "project": project,
                "timestamp": h.timestamp,
                "turn": h.turn,
                "role": if h.is_tool { "tool" } else { h.role.label() },
                "tool": h.is_tool,
                "match": h.snippet,
            });
            // Best-effort; ignore broken-pipe so piping into `head` stays quiet.
            let _ = writeln!(out, "{}", obj);
        }
    }
}

pub(super) fn emit_stats(results: &[SessionHits]) {
    let mut matches = 0usize;
    let mut first: Option<&str> = None;
    let mut last: Option<&str> = None;
    for r in results {
        for h in &r.hits {
            if h.kind != HitKind::Match {
                continue;
            }
            matches += 1;
            if let Some(ts) = h.timestamp.as_deref() {
                if first.is_none_or(|cur| ts < cur) {
                    first = Some(ts);
                }
                if last.is_none_or(|cur| ts > cur) {
                    last = Some(ts);
                }
            }
        }
    }
    let sessions = results.len();
    let head = format!(
        "{} {} · {} {}",
        paint(BOLD, &matches.to_string()),
        plural(matches, "match", "matches"),
        paint(BOLD, &sessions.to_string()),
        plural(sessions, "session", "sessions"),
    );
    match (first, last) {
        (Some(a), Some(b)) => {
            let range = format!("{} → {}", fmt_ts(a), fmt_ts(b));
            println!("{head} · {}", paint(DIM, &range));
        }
        _ => println!("{head}"),
    }
}

fn plural<'a>(n: usize, one: &'a str, many: &'a str) -> &'a str {
    if n == 1 {
        one
    } else {
        many
    }
}

pub(super) fn print_session(s: &SessionHits) {
    let id = s.path.file_stem().and_then(|s| s.to_str()).unwrap_or("?");
    let short = id.get(..8).unwrap_or(id);
    let proj = s
        .path
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|s| s.to_str())
        .unwrap_or("?");
    let ts = s
        .hits
        .iter()
        .find(|h| h.kind == HitKind::Match)
        .and_then(|h| h.timestamp.as_deref())
        .map(fmt_ts)
        .unwrap_or_default();

    println!(
        "{}  {}  {}",
        paint(BOLD, short),
        paint(DIM, &ts),
        paint(CYAN, proj)
    );
    if let Some(p) = &s.first_prompt {
        let flat = clean(p);
        let preview: String = flat.chars().take(110).collect();
        let ellipsis = if flat.chars().count() > 110 {
            "…"
        } else {
            ""
        };
        println!("  {}{}", paint(DIM, &preview), paint(DIM, ellipsis));
    }
    for h in &s.hits {
        let label = if h.is_tool {
            "tool".to_string()
        } else {
            h.role.label().to_string()
        };
        let padded = pad(&label, 9);
        let (tag, body) = if h.kind == HitKind::Context {
            (paint(DIM, &padded), paint(DIM, &h.snippet))
        } else {
            let colored = match h.role {
                Role::User if !h.is_tool => paint(GREEN_ROLE, &padded),
                Role::Assistant => paint(MAGENTA_ROLE, &padded),
                _ => paint(YELLOW, &padded),
            };
            (colored, h.snippet.clone())
        };
        let turn = paint(DIM, &fmt_turn(h.turn));
        println!("  {tag} {turn} {body}");
    }
    if let Some(first) = s
        .hits
        .iter()
        .find(|h| h.kind == HitKind::Match)
        .and_then(|h| h.turn)
    {
        let hint = format!("  → cch show {short} --turns {first}");
        println!("{}", paint(DIM, &hint));
    }
}

/// 4-wide, right-aligned: `  #7`, ` #42`, `#123`, or `  - ` when unknown.
fn fmt_turn(turn: Option<usize>) -> String {
    match turn {
        Some(n) => {
            let s = format!("#{n}");
            let pad = 4usize.saturating_sub(s.chars().count());
            format!("{}{s}", " ".repeat(pad))
        }
        None => "  - ".to_string(),
    }
}

fn pad(s: &str, width: usize) -> String {
    let mut out = String::from(s);
    while out.chars().count() < width {
        out.push(' ');
    }
    out
}

fn fmt_ts(ts: &str) -> String {
    let s = ts.replace('T', " ");
    s.chars().take(19).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pad_extends_shorter_strings_only() {
        assert_eq!(pad("abc", 6), "abc   ");
        assert_eq!(pad("abcdef", 3), "abcdef");
    }

    #[test]
    fn fmt_ts_truncates_and_swaps_t() {
        assert_eq!(fmt_ts("2026-04-23T10:30:15.123Z"), "2026-04-23 10:30:15");
        // Also works for short timestamps.
        assert_eq!(fmt_ts("2026-04-23"), "2026-04-23");
    }

    #[test]
    fn fmt_turn_pads_to_four_chars() {
        assert_eq!(fmt_turn(Some(7)).chars().count(), 4);
        assert_eq!(fmt_turn(Some(42)), " #42");
        assert_eq!(fmt_turn(Some(123)), "#123");
        // Overflow still safe — just wider than 4.
        assert!(fmt_turn(Some(9999)).ends_with("#9999"));
        assert_eq!(fmt_turn(None), "  - ");
    }
}
