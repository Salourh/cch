//! File scanning: turn numbering, context windows, role/turn/tool filters.

use std::path::{Path, PathBuf};

use crate::paths::{encode_cwd, projects_root};
use crate::timebounds::in_range;
use crate::transcript::{iter_events, Event, Part, Role};

use super::matcher::Matcher;
use super::opts::{Hit, HitKind, Opts};
use super::snippet::{clean, snippet, snippet_plain, SNIPPET_WIDTH};

pub(super) const MAX_MATCHES_PER_SESSION: usize = 5;

pub(super) fn collect_files(here: bool, project: Option<&Path>) -> anyhow::Result<Vec<PathBuf>> {
    let root = projects_root()?;
    let mut out = Vec::new();
    let scoped: Option<PathBuf> = if let Some(p) = project {
        Some(p.to_path_buf())
    } else if here {
        let cwd = std::env::current_dir()?.canonicalize()?;
        Some(root.join(encode_cwd(&cwd)))
    } else {
        None
    };
    if let Some(pdir) = scoped {
        if pdir.is_dir() {
            for e in std::fs::read_dir(&pdir)?.filter_map(|e| e.ok()) {
                let p = e.path();
                if p.extension().and_then(|s| s.to_str()) == Some("jsonl") {
                    out.push(p);
                }
            }
        }
        return Ok(out);
    }
    let Ok(rd) = std::fs::read_dir(&root) else {
        return Ok(out);
    };
    for proj in rd.filter_map(|e| e.ok()) {
        let pp = proj.path();
        if !pp.is_dir() {
            continue;
        }
        let Ok(files) = std::fs::read_dir(&pp) else {
            continue;
        };
        for f in files.filter_map(|e| e.ok()) {
            let fp = f.path();
            if fp.extension().and_then(|s| s.to_str()) == Some("jsonl") {
                out.push(fp);
            }
        }
    }
    Ok(out)
}

pub(super) fn scan_file(path: &Path, opts: &Opts, matcher: &Matcher) -> anyhow::Result<Vec<Hit>> {
    use std::collections::VecDeque;

    // Resolve a --turns window first: negatives need a known total, so we count
    // default-visible events in a cheap first pass. Pure-positive specs could
    // skip this, but the branching isn't worth the complexity.
    let turn_window: Option<(usize, usize)> = match &opts.turns {
        Some(spec) => {
            let total = count_visible(path)?;
            Some(spec.resolve(total)?)
        }
        None => None,
    };

    let mut hits: Vec<Hit> = Vec::new();
    let mut matches: usize = 0;
    // Numbering mirrors `cch show` default view, so `#N` in output is a valid
    // `--turns N` target. Incremented for every default-visible event, even
    // ones we filter out for this grep (otherwise sidechains/role filters
    // would shift the numbers out of sync with `cch show`).
    let mut turn: usize = 0;
    let mut ring: VecDeque<Hit> = VecDeque::with_capacity(opts.context_before.saturating_add(1));
    let mut remaining_after: usize = 0;

    for ev in iter_events(path)? {
        let visible = ev.is_default_visible();
        if visible {
            turn += 1;
        }

        // Global gates — apply to matches AND context rows. Sidechains and the
        // time window are user intent; System is skipped as noise.
        if ev.is_sidechain && !opts.include_sidechains {
            continue;
        }
        if !in_range(
            ev.timestamp.as_deref(),
            opts.after.as_deref(),
            opts.before.as_deref(),
        ) {
            continue;
        }
        if ev.role == Role::System {
            continue;
        }
        // --no-tools drops whole tool-only events (bash dumps, file reads) so
        // they don't show up as matches OR as context rows. Mixed events keep
        // their text parts; only the ToolResult parts are skipped in extract_hits.
        if opts.no_tools && ev.is_tool_only() {
            continue;
        }

        let hit_turn = if visible { Some(turn) } else { None };
        // `--role user` means "what the human typed" — reject events that only
        // carry tool_result payloads (JSON labels them `"role": "tool"`), and
        // skip tool_result parts inside mixed events. Mirrors `cch show --role user`.
        let role_allows_match = opts
            .role
            .is_none_or(|r| ev.role == r && !(r == Role::User && ev.is_tool_only()));
        let turn_allows_match =
            turn_window.is_none_or(|(lo, hi)| hit_turn.is_some_and(|t| t >= lo && t <= hi));
        let skip_tool_parts = opts.no_tools || opts.role == Some(Role::User);

        let mut matched = false;
        if role_allows_match && turn_allows_match && matches < MAX_MATCHES_PER_SESSION {
            let before = hits.len();
            let mut ev_hits = Vec::new();
            extract_hits(
                &ev,
                matcher,
                skip_tool_parts,
                hit_turn,
                opts.json,
                &mut ev_hits,
            );
            if !ev_hits.is_empty() {
                // Flush the -B buffer immediately before the match rows.
                while let Some(c) = ring.pop_front() {
                    hits.push(c);
                }
                for h in ev_hits {
                    if matches >= MAX_MATCHES_PER_SESSION {
                        break;
                    }
                    hits.push(h);
                    matches += 1;
                }
                matched = hits.len() > before;
            }
        }

        if matched {
            remaining_after = opts.context_after;
        } else if remaining_after > 0 {
            // Trailing context from a previous match.
            hits.push(context_hit(&ev, hit_turn));
            remaining_after -= 1;
        } else if opts.context_before > 0 {
            // Keep this event as a candidate for -B context of a future match.
            ring.push_back(context_hit(&ev, hit_turn));
            while ring.len() > opts.context_before {
                ring.pop_front();
            }
        }

        if matches >= MAX_MATCHES_PER_SESSION && remaining_after == 0 {
            break;
        }
        // -l mode only cares whether ANY match exists in the session. Once we
        // have one, skip the rest of the file — saves a full scan on big sessions.
        if opts.files_with_matches && matches > 0 {
            break;
        }
        // Past the --turns window — no further matches possible; stop once we've
        // flushed any pending -A context.
        if let Some((_, hi)) = turn_window {
            if visible && turn > hi && remaining_after == 0 {
                break;
            }
        }
    }
    Ok(hits)
}

fn count_visible(path: &Path) -> anyhow::Result<usize> {
    let mut n = 0;
    for ev in iter_events(path)? {
        if ev.is_default_visible() {
            n += 1;
        }
    }
    Ok(n)
}

fn context_hit(ev: &Event, turn: Option<usize>) -> Hit {
    Hit {
        kind: HitKind::Context,
        role: ev.role,
        is_tool: ev.is_tool_only(),
        timestamp: ev.timestamp.clone(),
        turn,
        snippet: context_snippet(&event_preview(ev)),
    }
}

fn event_preview(ev: &Event) -> String {
    for part in &ev.parts {
        match part {
            Part::Text(s) | Part::ToolResult(s) if !s.trim().is_empty() => return s.clone(),
            Part::ToolUse { name, summary, .. } => {
                return if summary.is_empty() {
                    format!("[{name}]")
                } else {
                    format!("[{name}: {summary}]")
                };
            }
            _ => {}
        }
    }
    String::new()
}

fn context_snippet(text: &str) -> String {
    let flat = clean(text);
    let mut out: String = flat.chars().take(SNIPPET_WIDTH).collect();
    if flat.chars().count() > SNIPPET_WIDTH {
        out.push('…');
    }
    out
}

fn extract_hits(
    ev: &Event,
    matcher: &Matcher,
    no_tools: bool,
    turn: Option<usize>,
    plain: bool,
    hits: &mut Vec<Hit>,
) {
    for part in &ev.parts {
        let is_tool = matches!(part, Part::ToolResult(_));
        if no_tools && is_tool {
            continue;
        }
        let text = part.as_search_text();
        if text.is_empty() {
            continue;
        }
        let Some((pos, len)) = matcher.find(text) else {
            continue;
        };
        let snip = if plain {
            snippet_plain(text, pos, len)
        } else {
            snippet(text, pos, len)
        };
        hits.push(Hit {
            kind: HitKind::Match,
            role: ev.role,
            is_tool,
            timestamp: ev.timestamp.clone(),
            turn,
            snippet: snip,
        });
        if hits.len() >= MAX_MATCHES_PER_SESSION {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::show::TurnSpec;

    fn base_opts(pattern: &str) -> Opts {
        Opts {
            pattern: pattern.into(),
            here: false,
            project: None,
            case_sensitive: true,
            regex: false,
            role: None,
            include_sidechains: false,
            no_tools: false,
            after: None,
            before: None,
            context_before: 0,
            context_after: 0,
            turns: None,
            json: false,
            files_with_matches: false,
            reverse: false,
            stats: false,
        }
    }

    fn scan(path: &Path, opts: &Opts) -> anyhow::Result<Vec<Hit>> {
        let m = Matcher::build(&opts.pattern, opts.case_sensitive, opts.regex)?;
        scan_file(path, opts, &m)
    }

    fn lit(needle: &str) -> Matcher {
        Matcher::build(needle, true, false).unwrap()
    }

    fn write_jsonl(tag: &str, lines: &[&str]) -> (std::path::PathBuf, std::path::PathBuf) {
        use std::io::Write;
        let dir = std::env::temp_dir().join(format!(
            "cch-grep-{}-{}-{}",
            tag,
            std::process::id(),
            line!()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.jsonl");
        let mut f = std::fs::File::create(&path).unwrap();
        for l in lines {
            writeln!(f, "{}", l).unwrap();
        }
        drop(f);
        (dir, path)
    }

    #[test]
    fn context_before_emits_preceding_events() {
        let (dir, path) = write_jsonl(
            "ctx-b",
            &[
                r#"{"type":"user","message":{"content":"turn one"}}"#,
                r#"{"type":"assistant","message":{"content":"turn two"}}"#,
                r#"{"type":"user","message":{"content":"turn three"}}"#,
                r#"{"type":"assistant","message":{"content":"answer with NEEDLE"}}"#,
            ],
        );
        let opts = Opts {
            context_before: 2,
            ..base_opts("NEEDLE")
        };
        let hits = scan(&path, &opts).unwrap();
        assert_eq!(hits.len(), 3);
        assert_eq!(hits[0].kind, HitKind::Context);
        assert_eq!(hits[0].turn, Some(2));
        assert_eq!(hits[1].kind, HitKind::Context);
        assert_eq!(hits[1].turn, Some(3));
        assert_eq!(hits[2].kind, HitKind::Match);
        assert_eq!(hits[2].turn, Some(4));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn context_after_emits_following_events() {
        let (dir, path) = write_jsonl(
            "ctx-a",
            &[
                r#"{"type":"user","message":{"content":"prompt NEEDLE"}}"#,
                r#"{"type":"assistant","message":{"content":"reply one"}}"#,
                r#"{"type":"user","message":{"content":"followup"}}"#,
                r#"{"type":"assistant","message":{"content":"reply two"}}"#,
            ],
        );
        let opts = Opts {
            context_after: 2,
            ..base_opts("NEEDLE")
        };
        let hits = scan(&path, &opts).unwrap();
        assert_eq!(hits.len(), 3);
        assert_eq!(hits[0].kind, HitKind::Match);
        assert_eq!(hits[1].kind, HitKind::Context);
        assert!(hits[1].snippet.contains("reply one"));
        assert_eq!(hits[2].kind, HitKind::Context);
        assert!(hits[2].snippet.contains("followup"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn context_ignores_role_filter_but_respects_sidechain_gate() {
        // Role filter is assistant-only, but -C 1 should still show the user turn around it.
        let (dir, path) = write_jsonl(
            "ctx-role",
            &[
                r#"{"type":"user","message":{"content":"the question"}}"#,
                r#"{"type":"assistant","message":{"content":"answer with NEEDLE"}}"#,
                r#"{"type":"user","message":{"content":"the followup"}}"#,
            ],
        );
        let opts = Opts {
            role: Some(Role::Assistant),
            context_before: 1,
            context_after: 1,
            ..base_opts("NEEDLE")
        };
        let hits = scan(&path, &opts).unwrap();
        assert_eq!(hits.len(), 3);
        assert_eq!(hits[0].kind, HitKind::Context);
        assert_eq!(hits[0].role, Role::User);
        assert!(hits[0].snippet.contains("the question"));
        assert_eq!(hits[1].kind, HitKind::Match);
        assert_eq!(hits[2].kind, HitKind::Context);
        assert_eq!(hits[2].role, Role::User);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn context_overlapping_matches_dedupe() {
        // Two matches with -B 1: the second match's "before" is the first match itself,
        // so we should not repeat it as context.
        let (dir, path) = write_jsonl(
            "ctx-dedupe",
            &[
                r#"{"type":"user","message":{"content":"first NEEDLE"}}"#,
                r#"{"type":"assistant","message":{"content":"second NEEDLE"}}"#,
            ],
        );
        let opts = Opts {
            context_before: 1,
            ..base_opts("NEEDLE")
        };
        let hits = scan(&path, &opts).unwrap();
        let matches = hits.iter().filter(|h| h.kind == HitKind::Match).count();
        let context = hits.iter().filter(|h| h.kind == HitKind::Context).count();
        assert_eq!(matches, 2);
        assert_eq!(context, 0);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn context_preview_shows_tool_use_tag() {
        let ev = Event {
            role: Role::Assistant,
            is_sidechain: false,
            timestamp: None,
            parts: vec![Part::ToolUse {
                name: "Bash".into(),
                summary: "command=ls".into(),
                file_path: None,
            }],
        };
        assert_eq!(event_preview(&ev), "[Bash: command=ls]");
    }

    #[test]
    fn scan_file_json_mode_produces_plain_snippets() {
        let (dir, path) = write_jsonl(
            "json-plain",
            &[
                r#"{"type":"assistant","timestamp":"2026-04-24T10:00:00.000Z","message":{"content":"hit NEEDLE here"}}"#,
            ],
        );
        std::env::remove_var("NO_COLOR");
        let opts = Opts {
            json: true,
            ..base_opts("NEEDLE")
        };
        let hits = scan(&path, &opts).unwrap();
        assert_eq!(hits.len(), 1);
        assert!(!hits[0].snippet.contains('\x1b'));
        assert!(hits[0].snippet.contains("NEEDLE"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn scan_file_regex_mode_finds_alternation() {
        let (dir, path) = write_jsonl(
            "regex",
            &[
                r#"{"type":"user","message":{"content":"alpha line"}}"#,
                r#"{"type":"assistant","message":{"content":"bravo line"}}"#,
                r#"{"type":"user","message":{"content":"charlie line"}}"#,
            ],
        );
        let opts = Opts {
            regex: true,
            ..base_opts("alpha|charlie")
        };
        let hits = scan(&path, &opts).unwrap();
        assert_eq!(hits.len(), 2);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn extract_hits_respects_max_per_session() {
        let ev = Event {
            role: Role::Assistant,
            is_sidechain: false,
            timestamp: None,
            parts: (0..20)
                .map(|_| Part::Text("NEEDLE here NEEDLE".into()))
                .collect(),
        };
        let mut hits = Vec::new();
        extract_hits(&ev, &lit("NEEDLE"), false, Some(1), false, &mut hits);
        assert_eq!(hits.len(), MAX_MATCHES_PER_SESSION);
    }

    #[test]
    fn extract_hits_skips_tool_use_parts() {
        let ev = Event {
            role: Role::Assistant,
            is_sidechain: false,
            timestamp: None,
            parts: vec![
                Part::ToolUse {
                    name: "X".into(),
                    summary: "NEEDLE".into(),
                    file_path: None,
                },
                Part::Text("NEEDLE".into()),
            ],
        };
        let mut hits = Vec::new();
        extract_hits(&ev, &lit("NEEDLE"), false, Some(1), false, &mut hits);
        assert_eq!(hits.len(), 1);
        assert!(!hits[0].is_tool);
    }

    #[test]
    fn extract_hits_skips_tool_result_when_no_tools() {
        let ev = Event {
            role: Role::User,
            is_sidechain: false,
            timestamp: None,
            parts: vec![
                Part::ToolResult("NEEDLE in dump".into()),
                Part::Text("NEEDLE in text".into()),
            ],
        };
        let mut hits = Vec::new();
        extract_hits(&ev, &lit("NEEDLE"), true, Some(1), false, &mut hits);
        assert_eq!(hits.len(), 1);
        assert!(!hits[0].is_tool);
    }

    #[test]
    fn scan_file_no_tools_drops_tool_only_events() {
        let (dir, path) = write_jsonl(
            "no-tools",
            &[
                r#"{"type":"user","message":{"content":[{"type":"tool_result","content":"NEEDLE in bash dump"}]}}"#,
                r#"{"type":"assistant","message":{"content":"NEEDLE in reply"}}"#,
            ],
        );
        let opts = Opts {
            no_tools: true,
            ..base_opts("NEEDLE")
        };
        let hits = scan(&path, &opts).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].role, Role::Assistant);
        assert!(!hits[0].is_tool);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn extract_hits_marks_tool_result() {
        let ev = Event {
            role: Role::User,
            is_sidechain: false,
            timestamp: None,
            parts: vec![Part::ToolResult("NEEDLE".into())],
        };
        let mut hits = Vec::new();
        extract_hits(&ev, &lit("NEEDLE"), false, Some(1), false, &mut hits);
        assert_eq!(hits.len(), 1);
        assert!(hits[0].is_tool);
    }

    #[test]
    fn scan_file_respects_sidechain_flag() {
        use std::io::Write;
        let dir =
            std::env::temp_dir().join(format!("cch-grep-sc-{}-{}", std::process::id(), line!()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.jsonl");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(
            f,
            r#"{{"type":"assistant","isSidechain":true,"message":{{"content":"NEEDLE"}}}}"#
        )
        .unwrap();
        writeln!(
            f,
            r#"{{"type":"assistant","message":{{"content":"NEEDLE main"}}}}"#
        )
        .unwrap();
        drop(f);

        let hits = scan(&path, &base_opts("NEEDLE")).unwrap();
        assert_eq!(hits.len(), 1); // sidechain filtered

        let incl = Opts {
            include_sidechains: true,
            ..base_opts("NEEDLE")
        };
        let hits = scan(&path, &incl).unwrap();
        assert_eq!(hits.len(), 2);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn scan_file_role_filter_skips_non_matching_roles() {
        let (dir, path) = write_jsonl(
            "role",
            &[
                r#"{"type":"user","message":{"content":"NEEDLE from user"}}"#,
                r#"{"type":"assistant","message":{"content":"NEEDLE from assistant"}}"#,
            ],
        );
        let opts = Opts {
            role: Some(Role::Assistant),
            ..base_opts("NEEDLE")
        };
        let hits = scan(&path, &opts).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].role, Role::Assistant);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn role_user_excludes_tool_only_events() {
        // A `type:user` event whose only parts are tool_result payloads renders
        // as `tool` — `--role user` must NOT match inside it.
        let (dir, path) = write_jsonl(
            "role-user-tool-only",
            &[
                r#"{"type":"user","message":{"content":[{"type":"tool_result","content":"NEEDLE in bash output"}]}}"#,
                r#"{"type":"user","message":{"content":"NEEDLE typed by human"}}"#,
            ],
        );
        let opts = Opts {
            role: Some(Role::User),
            ..base_opts("NEEDLE")
        };
        let hits = scan(&path, &opts).unwrap();
        assert_eq!(hits.len(), 1);
        assert!(!hits[0].is_tool);
        assert!(hits[0].snippet.contains("typed by human"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn role_user_excludes_tool_result_parts_of_mixed_events() {
        // Rare but possible: a user event with both a tool_result and real text.
        // `--role user` must skip the tool_result part.
        let ev = Event {
            role: Role::User,
            is_sidechain: false,
            timestamp: None,
            parts: vec![
                Part::ToolResult("NEEDLE in dump".into()),
                Part::Text("NEEDLE in prompt".into()),
            ],
        };
        let mut hits = Vec::new();
        // skip_tool_parts=true simulates the grep flow when --role user is set.
        extract_hits(&ev, &lit("NEEDLE"), true, Some(1), false, &mut hits);
        assert_eq!(hits.len(), 1);
        assert!(!hits[0].is_tool);
        assert!(hits[0].snippet.contains("prompt"));
    }

    #[test]
    fn scan_file_default_excludes_system() {
        let (dir, path) = write_jsonl(
            "sys",
            &[r#"{"type":"system","message":{"content":"NEEDLE"}}"#],
        );
        assert!(scan(&path, &base_opts("NEEDLE")).unwrap().is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn in_range_handles_fractional_seconds() {
        // Transcript uses `...17:00:00.123Z`; bound is `...17:00:00`.
        // Naive full-string lex compare would put '.123Z' < 'Z' spuriously.
        // We only compare the first 19 chars, so they're equal.
        let ts = "2026-04-23T17:00:00.999Z";
        assert!(in_range(Some(ts), Some("2026-04-23T17:00:00"), None));
    }

    #[test]
    fn scan_file_numbers_turns_matching_cc_show() {
        let (dir, path) = write_jsonl(
            "turns",
            &[
                // 1: real user prompt
                r#"{"type":"user","message":{"content":"hello"}}"#,
                // wrapper — skipped in numbering
                r#"{"type":"user","message":{"content":"<system-reminder>x</system-reminder>"}}"#,
                // 2: assistant reply
                r#"{"type":"assistant","message":{"content":"first reply"}}"#,
                // sidechain — skipped in numbering
                r#"{"type":"assistant","isSidechain":true,"message":{"content":"NEEDLE in side"}}"#,
                // 3: assistant with the match
                r#"{"type":"assistant","message":{"content":"answer with NEEDLE inside"}}"#,
            ],
        );
        let hits = scan(&path, &base_opts("NEEDLE")).unwrap();
        assert_eq!(hits.len(), 1);
        // Sidechain is filtered out; the match lands in turn #3.
        assert_eq!(hits[0].turn, Some(3));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn turn_window_restricts_matches_to_range() {
        let (dir, path) = write_jsonl(
            "turns-last",
            &[
                r#"{"type":"user","message":{"content":"NEEDLE one"}}"#,
                r#"{"type":"assistant","message":{"content":"NEEDLE two"}}"#,
                r#"{"type":"user","message":{"content":"NEEDLE three"}}"#,
                r#"{"type":"assistant","message":{"content":"NEEDLE four"}}"#,
            ],
        );
        let opts = Opts {
            turns: Some(TurnSpec::Single(-1)),
            ..base_opts("NEEDLE")
        };
        let hits = scan(&path, &opts).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].turn, Some(4));
        assert!(hits[0].snippet.contains("four"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn turn_window_combined_with_role_acts_as_last_assistant() {
        // --turns -1 + --role assistant == "last assistant turn" when the last
        // turn is assistant; otherwise the window excludes any role match.
        let (dir, path) = write_jsonl(
            "turns-last-role",
            &[
                r#"{"type":"user","message":{"content":"NEEDLE one"}}"#,
                r#"{"type":"assistant","message":{"content":"NEEDLE two"}}"#,
                r#"{"type":"user","message":{"content":"NEEDLE three"}}"#,
            ],
        );
        let opts = Opts {
            turns: Some(TurnSpec::Single(-1)),
            role: Some(Role::Assistant),
            ..base_opts("NEEDLE")
        };
        assert!(scan(&path, &opts).unwrap().is_empty());

        let opts = Opts {
            turns: Some(TurnSpec::Single(-2)),
            role: Some(Role::Assistant),
            ..base_opts("NEEDLE")
        };
        let hits = scan(&path, &opts).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].turn, Some(2));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn turn_window_head_range_keeps_only_first_turns() {
        let (dir, path) = write_jsonl(
            "turns-head",
            &[
                r#"{"type":"user","message":{"content":"NEEDLE one"}}"#,
                r#"{"type":"assistant","message":{"content":"NEEDLE two"}}"#,
                r#"{"type":"user","message":{"content":"NEEDLE three"}}"#,
            ],
        );
        let opts = Opts {
            turns: Some(TurnSpec::Range(None, Some(2))),
            ..base_opts("NEEDLE")
        };
        let hits = scan(&path, &opts).unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].turn, Some(1));
        assert_eq!(hits[1].turn, Some(2));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn scan_file_time_range_filters_events() {
        let (dir, path) = write_jsonl(
            "time",
            &[
                r#"{"type":"user","timestamp":"2026-04-22T10:00:00.000Z","message":{"content":"NEEDLE before"}}"#,
                r#"{"type":"user","timestamp":"2026-04-23T16:59:00.000Z","message":{"content":"NEEDLE just before"}}"#,
                r#"{"type":"user","timestamp":"2026-04-23T18:00:00.000Z","message":{"content":"NEEDLE after"}}"#,
            ],
        );
        let opts = Opts {
            after: Some("2026-04-23T00:00:00".into()),
            before: Some("2026-04-23T17:00:00".into()),
            ..base_opts("NEEDLE")
        };
        let hits = scan(&path, &opts).unwrap();
        assert_eq!(hits.len(), 1);
        assert!(hits[0].snippet.contains("just before"));
        std::fs::remove_dir_all(&dir).ok();
    }
}
