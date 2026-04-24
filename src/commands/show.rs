use std::io::{BufWriter, Write};
use std::path::Path;

use crate::session::resolve_prefix;
use crate::term::{paint, BOLD, CYAN, DIM, GREEN, MAGENTA, YELLOW};
use crate::transcript::{iter_events, Event, Part, Role};

pub struct Opts {
    pub prefix: String,
    pub include_sidechains: bool,
    pub include_system: bool,
    /// Keep only turns with this displayed role (`user` excludes tool_result-only
    /// events, which render as `tool`). `None` = no filter.
    pub role: Option<Role>,
    pub turns: Option<String>,
}

pub fn run(opts: Opts) -> anyhow::Result<()> {
    let path = resolve_prefix(&opts.prefix)?;
    let id = path.file_stem().and_then(|s| s.to_str()).unwrap_or("?");
    let proj = path
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|s| s.to_str())
        .unwrap_or("?");

    let spec = opts
        .turns
        .as_deref()
        .map(TurnSpec::parse)
        .transpose()?
        .unwrap_or(TurnSpec::All);

    // Two-pass only when the spec references the end of the transcript (negative
    // indices) or a specific turn we need to validate. `All` streams in one pass
    // — that's the path that matters for giant transcripts.
    let (lo, hi, total_upfront) = if spec.needs_total() {
        let total = count_visible(&path, &opts)?;
        let (lo, hi) = spec.resolve(total)?;
        (lo, hi, Some(total))
    } else {
        let (lo, hi) = spec.resolve_open();
        (lo, hi, None)
    };

    let stdout = std::io::stdout();
    let mut out = BufWriter::with_capacity(64 * 1024, stdout.lock());
    writeln!(out, "{}  {}", paint(BOLD, id), paint(CYAN, proj))?;

    let mut printed_any = false;
    let mut seen = 0usize;
    for ev in iter_events(&path)? {
        if !is_visible(&ev, &opts) {
            continue;
        }
        seen += 1;
        if seen > hi {
            if total_upfront.is_some() {
                break;
            }
            continue;
        }
        if seen < lo {
            continue;
        }
        if printed_any {
            writeln!(out)?;
        }
        printed_any = true;
        print_event(&mut out, &ev, seen)?;
    }

    let total = total_upfront.unwrap_or(seen);
    let hi_display = hi.min(total);
    print_footer(&mut out, lo, hi_display, total)?;
    out.flush()?;
    Ok(())
}

fn count_visible(path: &Path, opts: &Opts) -> anyhow::Result<usize> {
    Ok(iter_events(path)?.filter(|ev| is_visible(ev, opts)).count())
}

fn is_visible(ev: &Event, opts: &Opts) -> bool {
    if ev.parts.is_empty() {
        return false;
    }
    if ev.is_sidechain && !opts.include_sidechains {
        return false;
    }
    if ev.role == Role::System && !opts.include_system {
        return false;
    }
    if let Some(r) = opts.role {
        // `--role user` should exclude tool_result-only events (they render as
        // `tool`, not `user`). For `assistant` the tool-only check is a no-op.
        if ev.role != r || ev.is_tool_only() {
            return false;
        }
    }
    !is_wrapper_user_event(ev)
}

/// Hide pure-wrapper user events (e.g. `<system-reminder>`-only messages).
fn is_wrapper_user_event(ev: &Event) -> bool {
    if ev.role != Role::User {
        return false;
    }
    if !ev.parts.iter().all(|p| matches!(p, Part::Text(_))) {
        return false;
    }
    ev.parts.iter().all(|p| match p {
        Part::Text(s) => {
            let t = s.trim();
            t.is_empty() || t.starts_with('<')
        }
        _ => false,
    })
}

fn print_event<W: Write>(out: &mut W, ev: &Event, turn: usize) -> std::io::Result<()> {
    let label = if ev.is_tool_only() {
        "tool"
    } else {
        ev.role.label()
    };
    let color = match (ev.role, ev.is_tool_only()) {
        (_, true) => YELLOW,
        (Role::User, _) => GREEN,
        (Role::Assistant, _) => MAGENTA,
        _ => DIM,
    };
    let ts = ev.timestamp.as_deref().map(fmt_ts).unwrap_or_default();
    let header = if ts.is_empty() {
        format!("── #{turn} {label} ──")
    } else {
        format!("── #{turn} {label} · {ts} ──")
    };
    writeln!(out, "{}", paint(color, &header))?;

    for p in &ev.parts {
        match p {
            Part::Text(s) => writeln!(out, "{}", s)?,
            Part::ToolUse { name, summary, .. } => {
                let line = if summary.is_empty() {
                    format!("[tool_use] {name}")
                } else {
                    format!("[tool_use] {name}: {summary}")
                };
                writeln!(out, "{}", paint(DIM, &line))?;
            }
            Part::ToolResult(s) => writeln!(out, "{}", s)?,
        }
    }
    Ok(())
}

fn print_footer<W: Write>(out: &mut W, lo: usize, hi: usize, total: usize) -> std::io::Result<()> {
    if total == 0 {
        writeln!(out, "{}", paint(DIM, "── 0 turns ──"))?;
        return Ok(());
    }
    let txt = if lo == 1 && hi == total {
        format!("── {total} turn{} ──", if total == 1 { "" } else { "s" })
    } else {
        format!("── turns {lo}–{hi} of {total} ──")
    };
    writeln!(out)?;
    writeln!(out, "{}", paint(DIM, &txt))?;
    Ok(())
}

fn fmt_ts(ts: &str) -> String {
    let s = ts.replace('T', " ");
    s.chars().take(19).collect()
}

/// Python-slice-ish turn selector. 1-indexed; negatives count from the end.
#[derive(Debug, PartialEq)]
pub enum TurnSpec {
    All,
    Single(i64),
    Range(Option<i64>, Option<i64>),
}

impl TurnSpec {
    pub fn parse(s: &str) -> anyhow::Result<Self> {
        let s = s.trim();
        if s.is_empty() {
            anyhow::bail!("empty --turns spec");
        }
        if let Some((a, b)) = s.split_once("..") {
            let lo = if a.is_empty() {
                None
            } else {
                Some(parse_i64(a)?)
            };
            let hi = if b.is_empty() {
                None
            } else {
                Some(parse_i64(b)?)
            };
            if lo == Some(0) || hi == Some(0) {
                anyhow::bail!("--turns: 0 is not a valid turn (use 1-indexed)");
            }
            Ok(TurnSpec::Range(lo, hi))
        } else {
            let n = parse_i64(s)?;
            if n == 0 {
                anyhow::bail!("--turns: 0 is not a valid turn (use 1-indexed)");
            }
            Ok(TurnSpec::Single(n))
        }
    }

    /// True when the spec requires knowing the total number of visible turns
    /// before streaming (negative indices, or a single turn we need to validate).
    pub fn needs_total(&self) -> bool {
        match *self {
            TurnSpec::All => false,
            TurnSpec::Single(_) => true,
            TurnSpec::Range(lo, hi) => lo.is_some_and(|n| n < 0) || hi.is_some_and(|n| n < 0),
        }
    }

    /// Resolve to a streaming `(lo, hi)` without knowing the total. Only valid
    /// when `needs_total()` is false — `hi` may be `usize::MAX` meaning open.
    pub fn resolve_open(&self) -> (usize, usize) {
        match *self {
            TurnSpec::All => (1, usize::MAX),
            TurnSpec::Range(lo, hi) => {
                let lo = lo.map(|n| n as usize).unwrap_or(1).max(1);
                let hi = hi.map(|n| n as usize).unwrap_or(usize::MAX);
                if lo > hi {
                    (1, 0)
                } else {
                    (lo, hi)
                }
            }
            TurnSpec::Single(_) => unreachable!("Single requires total upfront"),
        }
    }

    /// Resolve to an inclusive 1-indexed `(lo, hi)` clamped to `total`.
    /// Empty selections (lo > hi or total == 0) are returned as `(1, 0)`.
    pub fn resolve(&self, total: usize) -> anyhow::Result<(usize, usize)> {
        if total == 0 {
            return Ok((1, 0));
        }
        let resolve_one = |n: i64| -> usize {
            if n > 0 {
                (n as usize).min(total)
            } else {
                // negative: -1 → total, -2 → total-1, ...
                let from_end = (-n) as usize;
                total.saturating_sub(from_end.saturating_sub(1)).max(1)
            }
        };
        match *self {
            TurnSpec::All => Ok((1, total)),
            TurnSpec::Single(n) => {
                let abs = n.unsigned_abs() as usize;
                if abs > total {
                    anyhow::bail!(
                        "--turns: {n} is out of range (transcript has {total} turn{})",
                        if total == 1 { "" } else { "s" }
                    );
                }
                let r = resolve_one(n);
                Ok((r, r))
            }
            TurnSpec::Range(lo, hi) => {
                let lo = lo.map(resolve_one).unwrap_or(1);
                let hi = hi.map(resolve_one).unwrap_or(total);
                if lo > hi {
                    Ok((1, 0))
                } else {
                    Ok((lo, hi))
                }
            }
        }
    }
}

fn parse_i64(s: &str) -> anyhow::Result<i64> {
    s.trim()
        .parse::<i64>()
        .map_err(|_| anyhow::anyhow!("--turns: invalid number {s:?}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(role: Role, parts: Vec<Part>) -> Event {
        Event {
            role,
            is_sidechain: false,
            timestamp: None,
            parts,
        }
    }

    fn opts_with_role(role: Option<Role>) -> Opts {
        Opts {
            prefix: String::new(),
            include_sidechains: false,
            include_system: false,
            role,
            turns: None,
        }
    }

    #[test]
    fn role_filter_user_keeps_user_text() {
        let e = ev(Role::User, vec![Part::Text("hi".into())]);
        assert!(is_visible(&e, &opts_with_role(Some(Role::User))));
    }

    #[test]
    fn role_filter_user_drops_assistant() {
        let e = ev(Role::Assistant, vec![Part::Text("reply".into())]);
        assert!(!is_visible(&e, &opts_with_role(Some(Role::User))));
    }

    #[test]
    fn role_filter_user_drops_tool_result_events() {
        // `type:user` event that only carries a tool_result — displayed as `tool`.
        let e = ev(Role::User, vec![Part::ToolResult("output".into())]);
        assert!(!is_visible(&e, &opts_with_role(Some(Role::User))));
    }

    #[test]
    fn role_filter_assistant_keeps_assistant_with_tool_use() {
        let e = ev(
            Role::Assistant,
            vec![
                Part::Text("ok".into()),
                Part::ToolUse {
                    name: "Bash".into(),
                    summary: "command=ls".into(),
                    file_path: None,
                },
            ],
        );
        assert!(is_visible(&e, &opts_with_role(Some(Role::Assistant))));
    }

    #[test]
    fn role_filter_none_keeps_everything_visible() {
        let e = ev(Role::User, vec![Part::ToolResult("r".into())]);
        assert!(is_visible(&e, &opts_with_role(None)));
    }

    #[test]
    fn role_filter_user_still_drops_wrapper_prompts() {
        let e = ev(
            Role::User,
            vec![Part::Text("<system-reminder>x</system-reminder>".into())],
        );
        assert!(!is_visible(&e, &opts_with_role(Some(Role::User))));
    }

    #[test]
    fn wrapper_detection_hides_system_reminder_only() {
        let e = ev(
            Role::User,
            vec![Part::Text("<system-reminder>x</system-reminder>".into())],
        );
        assert!(is_wrapper_user_event(&e));
    }

    #[test]
    fn wrapper_detection_keeps_real_text() {
        let e = ev(Role::User, vec![Part::Text("hey".into())]);
        assert!(!is_wrapper_user_event(&e));
    }

    #[test]
    fn wrapper_detection_keeps_mixed_parts() {
        let e = ev(
            Role::User,
            vec![Part::Text("<wrapper>".into()), Part::ToolResult("r".into())],
        );
        assert!(!is_wrapper_user_event(&e));
    }

    #[test]
    fn wrapper_detection_ignores_assistant() {
        let e = ev(Role::Assistant, vec![Part::Text("<x>".into())]);
        assert!(!is_wrapper_user_event(&e));
    }

    #[test]
    fn wrapper_detection_all_empty_text_is_wrapper() {
        let e = ev(
            Role::User,
            vec![Part::Text("   ".into()), Part::Text("".into())],
        );
        assert!(is_wrapper_user_event(&e));
    }

    #[test]
    fn fmt_ts_truncates_to_19_chars() {
        assert_eq!(fmt_ts("2026-04-23T09:15:30.000Z"), "2026-04-23 09:15:30");
    }

    // --- TurnSpec ----------------------------------------------------------

    #[test]
    fn spec_parses_single() {
        assert_eq!(TurnSpec::parse("5").unwrap(), TurnSpec::Single(5));
        assert_eq!(TurnSpec::parse("-1").unwrap(), TurnSpec::Single(-1));
    }

    #[test]
    fn spec_parses_ranges() {
        assert_eq!(
            TurnSpec::parse("1..5").unwrap(),
            TurnSpec::Range(Some(1), Some(5))
        );
        assert_eq!(
            TurnSpec::parse("..5").unwrap(),
            TurnSpec::Range(None, Some(5))
        );
        assert_eq!(
            TurnSpec::parse("5..").unwrap(),
            TurnSpec::Range(Some(5), None)
        );
        assert_eq!(
            TurnSpec::parse("-5..").unwrap(),
            TurnSpec::Range(Some(-5), None)
        );
        assert_eq!(TurnSpec::parse("..").unwrap(), TurnSpec::Range(None, None));
    }

    #[test]
    fn spec_rejects_zero_and_garbage() {
        assert!(TurnSpec::parse("0").is_err());
        assert!(TurnSpec::parse("0..5").is_err());
        assert!(TurnSpec::parse("abc").is_err());
        assert!(TurnSpec::parse("").is_err());
    }

    #[test]
    fn resolve_all_returns_full_range() {
        assert_eq!(TurnSpec::All.resolve(10).unwrap(), (1, 10));
    }

    #[test]
    fn resolve_single_positive_and_negative() {
        assert_eq!(TurnSpec::Single(3).resolve(10).unwrap(), (3, 3));
        assert_eq!(TurnSpec::Single(-1).resolve(10).unwrap(), (10, 10));
        assert_eq!(TurnSpec::Single(-3).resolve(10).unwrap(), (8, 8));
        // Out-of-range single indices error (user asked for a specific turn
        // that doesn't exist — silently showing another one would mask typos).
        assert!(TurnSpec::Single(99).resolve(10).is_err());
        assert!(TurnSpec::Single(-99).resolve(10).is_err());
    }

    #[test]
    fn resolve_ranges() {
        let r = TurnSpec::Range(Some(2), Some(4)).resolve(10).unwrap();
        assert_eq!(r, (2, 4));
        // head
        assert_eq!(TurnSpec::Range(None, Some(3)).resolve(10).unwrap(), (1, 3));
        // tail via negative lo
        assert_eq!(
            TurnSpec::Range(Some(-3), None).resolve(10).unwrap(),
            (8, 10)
        );
        // open both ways
        assert_eq!(TurnSpec::Range(None, None).resolve(10).unwrap(), (1, 10));
    }

    #[test]
    fn resolve_empty_when_lo_gt_hi() {
        let r = TurnSpec::Range(Some(5), Some(2)).resolve(10).unwrap();
        assert_eq!(r, (1, 0));
    }

    #[test]
    fn resolve_empty_transcript() {
        assert_eq!(TurnSpec::All.resolve(0).unwrap(), (1, 0));
        assert_eq!(TurnSpec::Single(1).resolve(0).unwrap(), (1, 0));
    }
}
