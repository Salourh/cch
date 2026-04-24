use std::path::PathBuf;

use crate::commands::blame;
use crate::paths::{encode_cwd, project_dir, projects_root};
use crate::session::{list_sessions, SessionEntry};
use crate::timebounds::{format_systime_utc, in_range};
use crate::transcript::iter_events;

const LIST_LIMIT: usize = 10;
const PREVIEW_WIDTH: usize = 80;

pub struct Opts {
    pub count: Option<usize>,
    pub include_empty: bool,
    /// Normalized `YYYY-MM-DDTHH:MM:SS` (inclusive).
    pub after: Option<String>,
    /// Normalized `YYYY-MM-DDTHH:MM:SS` (exclusive).
    pub before: Option<String>,
    /// Project dir to list (already resolved). `None` = current project.
    pub project: Option<PathBuf>,
    /// Filter to sessions that edited this path (file or directory).
    pub touched: Option<String>,
    /// Filter to sessions that produced this commit (reverse `cch blame`).
    /// Forces the listing into the commit's repo project dir.
    pub produced_commit: Option<String>,
    /// Clip each expanded prompt to this many lines (only meaningful with `-n`).
    pub head: Option<usize>,
}

/// True if `edited` (a path written by an Edit/Write/MultiEdit/NotebookEdit call)
/// matches the user's `query`. Three ways to match:
/// - equal (after light normalization)
/// - `query` is a directory prefix of `edited` (ends with `/`)
/// - `edited` ends with `/query` (so `foo.rs` matches `/a/b/foo.rs`)
fn path_matches(edited: &str, query: &str) -> bool {
    if edited == query {
        return true;
    }
    let q = query.trim_end_matches('/');
    // Directory prefix rooted at the start: `/a/b` matches `/a/b/c.rs`.
    let dir = format!("{q}/");
    if edited.starts_with(&dir) {
        return true;
    }
    // Directory segment anywhere in the path: `src/commands` matches
    // `/home/u/repo/src/commands/session.rs`. Boundary on both sides.
    let mid = format!("/{q}/");
    if edited.contains(&mid) {
        return true;
    }
    // Tail match at a path boundary: `foo.rs` matches `/a/b/foo.rs`.
    let tail = if query.starts_with('/') {
        q.to_string()
    } else {
        format!("/{q}")
    };
    edited.ends_with(&tail)
}

fn session_touched(path: &std::path::Path, query: &str) -> bool {
    let Ok(events) = iter_events(path) else {
        return false;
    };
    for ev in events {
        for fp in ev.edited_paths() {
            if path_matches(fp, query) {
                return true;
            }
        }
    }
    false
}

pub fn run(opts: Opts) -> anyhow::Result<()> {
    // `--produced-commit` locks the listing to the commit's repo project dir
    // (overrides --project/default, since sessions for other repos can't have
    // produced this commit anyway).
    let produced = opts
        .produced_commit
        .as_deref()
        .map(blame::load_commit)
        .transpose()?;

    let pdir = match (&produced, &opts.project) {
        (Some(c), _) => projects_root()?.join(encode_cwd(&c.repo_root)),
        (None, Some(p)) => p.clone(),
        (None, None) => project_dir()?,
    };
    let mut sessions = list_sessions(&pdir, opts.include_empty)?;

    if opts.after.is_some() || opts.before.is_some() {
        sessions.retain(|s| {
            let key = format_systime_utc(s.mtime);
            in_range(Some(&key), opts.after.as_deref(), opts.before.as_deref())
        });
    }
    let touched_query = opts
        .touched
        .as_deref()
        .map(|q| q.trim_end_matches('/').to_string());
    if let Some(q) = touched_query.as_deref() {
        sessions.retain(|s| session_touched(&s.path, q));
    }
    if let Some(commit) = &produced {
        let (candidates, _) = blame::rank_candidates(&pdir, commit)?;
        use std::collections::HashSet;
        let keep: HashSet<PathBuf> = candidates.into_iter().map(|c| c.path).collect();
        sessions.retain(|s| keep.contains(&s.path));
    }

    if sessions.is_empty() {
        let loc: std::path::PathBuf = match &opts.project {
            Some(p) => p.clone(),
            None => std::env::current_dir()?,
        };
        let dated = opts.after.is_some() || opts.before.is_some();
        if let Some(commit) = &produced {
            eprintln!(
                "No sessions produced commit {} in {}",
                commit.short_sha,
                loc.display()
            );
        } else if let Some(q) = touched_query.as_deref() {
            eprintln!("No sessions touched {q:?} in {}", loc.display());
        } else if dated {
            eprintln!("No sessions in that window for {}", loc.display());
        } else {
            eprintln!("No sessions found for {}", loc.display());
        }
        std::process::exit(1);
    }

    if let Some(0) = opts.head {
        eprintln!("--head must be >= 1");
        std::process::exit(2);
    }
    match opts.count {
        Some(n) if n >= 1 => print_full(&sessions, n, opts.head),
        Some(_) => {
            eprintln!("Count must be >= 1");
            std::process::exit(2);
        }
        None => print_list(&sessions),
    }
    Ok(())
}

fn print_list(sessions: &[SessionEntry]) {
    let total = sessions.len();
    let shown = total.min(LIST_LIMIT);
    for (i, s) in sessions.iter().take(shown).enumerate() {
        let n = i + 1;
        let preview = s
            .first_prompt
            .as_deref()
            .map(|p| truncate(p, PREVIEW_WIDTH))
            .unwrap_or_else(|| "(no user prompt)".to_string());
        println!("-{:<2} {}  {}", n, s.id(), preview);
    }
    if total > shown {
        let more = total - shown;
        println!("… {more} older — `cch session -n {total}` to see all");
    }
}

fn print_full(sessions: &[SessionEntry], n: usize, head: Option<usize>) {
    let take = n.min(sessions.len());
    println!("Les {take} sessions les plus récentes.");
    println!();
    for (i, s) in sessions.iter().take(take).enumerate() {
        if i > 0 {
            println!();
            println!("{}", "-".repeat(72));
            println!();
        }
        println!("[-{}] {}", i + 1, s.id());
        if let Some(p) = &s.first_prompt {
            println!();
            match head {
                Some(limit) => {
                    let (clipped, dropped) = clip_lines(p, limit);
                    println!("{clipped}");
                    if dropped > 0 {
                        let id = s.id();
                        println!(
                            "… {dropped} more line{} — `cch show {id}` for the full prompt",
                            if dropped == 1 { "" } else { "s" }
                        );
                    }
                }
                None => println!("{p}"),
            }
        }
    }
}

/// Keep the first `limit` lines of `text`. Returns the clipped string and the
/// number of lines that were dropped (0 if nothing was clipped).
fn clip_lines(text: &str, limit: usize) -> (String, usize) {
    let total = text.lines().count();
    if total <= limit {
        return (text.to_string(), 0);
    }
    let kept: Vec<&str> = text.lines().take(limit).collect();
    (kept.join("\n"), total - limit)
}

fn truncate(text: &str, width: usize) -> String {
    let flat: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= width {
        flat
    } else {
        let mut out: String = flat.chars().take(width - 1).collect();
        out.push('…');
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_collapses_whitespace() {
        assert_eq!(truncate("a   b\nc\td", 80), "a b c d");
    }

    #[test]
    fn truncate_shorter_than_width_is_returned_as_is() {
        assert_eq!(truncate("short", 80), "short");
    }

    #[test]
    fn truncate_clips_with_ellipsis() {
        let long = "x".repeat(200);
        let out = truncate(&long, 10);
        // 9 chars + ellipsis = 10
        assert_eq!(out.chars().count(), 10);
        assert!(out.ends_with('…'));
    }

    #[test]
    fn truncate_handles_multibyte_safely() {
        let s = "é".repeat(100);
        let out = truncate(&s, 10);
        assert_eq!(out.chars().count(), 10);
        assert!(out.ends_with('…'));
    }

    #[test]
    fn path_matches_exact() {
        assert!(path_matches("/a/b/foo.rs", "/a/b/foo.rs"));
    }

    #[test]
    fn path_matches_directory_prefix() {
        assert!(path_matches("/a/b/c/foo.rs", "/a/b"));
        assert!(path_matches("/a/b/c/foo.rs", "/a/b/"));
        // Must be a path boundary, not a string prefix:
        assert!(!path_matches("/abc/foo.rs", "/ab"));
    }

    #[test]
    fn path_matches_basename_tail() {
        assert!(path_matches("/a/b/foo.rs", "foo.rs"));
        assert!(path_matches("/a/b/foo.rs", "b/foo.rs"));
        // Must be at a path boundary:
        assert!(!path_matches("/a/b/barfoo.rs", "foo.rs"));
    }

    #[test]
    fn path_matches_relative_directory_prefix() {
        // Regression: `cch session --touched src/commands` silently matched
        // nothing because `path_matches` only handled absolute dir prefixes
        // and basename tails. The help advertises "any file under PATH when
        // PATH is a directory" — relative dirs must match at a path boundary.
        assert!(path_matches(
            "/home/u/repo/src/commands/session.rs",
            "src/commands"
        ));
        assert!(path_matches(
            "/home/u/repo/src/commands/session.rs",
            "src/commands/"
        ));
        // Boundary still required — substring of a segment must not match.
        assert!(!path_matches(
            "/home/u/repo/src/commandsX/session.rs",
            "src/commands"
        ));
    }

    #[test]
    fn path_matches_unrelated() {
        assert!(!path_matches("/x/y/z.rs", "/a/b"));
        assert!(!path_matches("/x/y/z.rs", "w.rs"));
    }

    #[test]
    fn session_touched_reads_edit_tool_uses() {
        use std::io::Write;
        let dir =
            std::env::temp_dir().join(format!("cch-touched-{}-{}", std::process::id(), line!()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("s.jsonl");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, r#"{{"type":"user","message":{{"content":"hi"}}}}"#).unwrap();
        writeln!(
            f,
            r#"{{"type":"assistant","message":{{"content":[{{"type":"tool_use","name":"Edit","input":{{"file_path":"/repo/src/main.rs"}}}}]}}}}"#
        ).unwrap();
        drop(f);
        assert!(session_touched(&path, "/repo/src/main.rs"));
        assert!(session_touched(&path, "main.rs"));
        assert!(session_touched(&path, "/repo/src"));
        assert!(!session_touched(&path, "other.rs"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn session_touched_ignores_read_only_tools() {
        use std::io::Write;
        let dir =
            std::env::temp_dir().join(format!("cch-touched-ro-{}-{}", std::process::id(), line!()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("s.jsonl");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(
            f,
            r#"{{"type":"assistant","message":{{"content":[{{"type":"tool_use","name":"Read","input":{{"file_path":"/repo/src/main.rs"}}}}]}}}}"#
        ).unwrap();
        drop(f);
        assert!(!session_touched(&path, "/repo/src/main.rs"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn clip_lines_under_limit_is_unchanged() {
        let (out, dropped) = clip_lines("a\nb\nc", 5);
        assert_eq!(out, "a\nb\nc");
        assert_eq!(dropped, 0);
    }

    #[test]
    fn clip_lines_at_exact_limit_is_unchanged() {
        let (out, dropped) = clip_lines("a\nb\nc", 3);
        assert_eq!(out, "a\nb\nc");
        assert_eq!(dropped, 0);
    }

    #[test]
    fn clip_lines_over_limit_drops_tail() {
        let (out, dropped) = clip_lines("a\nb\nc\nd\ne", 2);
        assert_eq!(out, "a\nb");
        assert_eq!(dropped, 3);
    }

    #[test]
    fn truncate_at_exact_width() {
        let s = "a".repeat(80);
        assert_eq!(truncate(&s, 80), s);
    }
}
