//! `cch commits <session>` — list the git commits a session produced.
//!
//! Inverse of `cch blame <sha>`: given a session id (prefix), find the commits
//! in its repo that this session authored. Uses the same authorship signal
//! blame uses — a `git commit` Bash tool_use whose argv carries the commit's
//! subject or SHA.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::commands::blame::{load_commit_in, scan_one, Candidate, Commit};
use crate::paths::encode_cwd;
use crate::session::resolve_prefix;
use crate::term::{paint, BOLD, DIM, GREEN, YELLOW};
use crate::transcript::{iter_events, Part};

pub struct Opts {
    /// Session id or unique prefix.
    pub prefix: String,
    /// Include weaker matches (subject/sha/time only). Default is authored-only
    /// because the command answers "what did this session commit" rather than
    /// "what did it discuss".
    pub all: bool,
}

pub fn run(opts: Opts) -> anyhow::Result<i32> {
    let session = resolve_prefix(&opts.prefix)?;
    let repo = session_repo_root(&session)?;
    let (first_ts, last_ts) = session_window(&session)?;
    let shas = list_commits_in_window(&repo, first_ts.as_deref(), last_ts.as_deref())?;
    let id = short_id(&session);

    if shas.is_empty() {
        eprintln!(
            "No commits in {} within this session's activity window.",
            repo.display()
        );
        return Ok(1);
    }

    let mut rows: Vec<(Commit, Candidate)> = Vec::new();
    for sha in shas {
        let commit = load_commit_in(Some(&repo), &sha)?;
        let cand = scan_one(&session, &commit)?;
        if cand.score() == 0 {
            continue;
        }
        if !opts.all && !cand.authored_commit {
            continue;
        }
        rows.push((commit, cand));
    }

    if rows.is_empty() {
        if opts.all {
            eprintln!("No commits linked to session {id} in {}.", repo.display());
        } else {
            eprintln!(
                "No commits authored by session {id} in {} — try `cch commits {id} --all` for weaker matches.",
                repo.display()
            );
        }
        return Ok(1);
    }

    print_rows(&id, &repo, &rows);
    Ok(0)
}

fn short_id(path: &Path) -> String {
    path.file_stem()
        .and_then(|s| s.to_str())
        .and_then(|s| s.get(..8).map(str::to_string))
        .unwrap_or_default()
}

/// Find the git repo this session worked in. Two strategies:
/// 1. Any tool_use with an absolute `file_path` → climb to nearest `.git`.
/// 2. Fallback: current cwd's repo, iff its encoded-cwd matches the session's
///    project dir name (so we don't accidentally inspect an unrelated repo).
fn session_repo_root(session: &Path) -> anyhow::Result<PathBuf> {
    for ev in iter_events(session)? {
        for part in &ev.parts {
            if let Part::ToolUse {
                file_path: Some(fp),
                ..
            } = part
            {
                if fp.starts_with('/') {
                    if let Some(root) = climb_to_git(Path::new(fp)) {
                        return Ok(root);
                    }
                }
            }
        }
    }
    if let Ok(out) = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
    {
        if out.status.success() {
            let raw = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !raw.is_empty() {
                let root = PathBuf::from(&raw).canonicalize()?;
                let proj = session
                    .parent()
                    .and_then(|p| p.file_name())
                    .and_then(|s| s.to_str())
                    .unwrap_or("");
                if encode_cwd(&root) == proj {
                    return Ok(root);
                }
            }
        }
    }
    anyhow::bail!(
        "couldn't locate the git repo for this session — \
         run `cch commits` from inside that repo"
    )
}

fn climb_to_git(start: &Path) -> Option<PathBuf> {
    let mut cur = start;
    while let Some(parent) = cur.parent() {
        if parent.join(".git").exists() {
            return Some(parent.to_path_buf());
        }
        cur = parent;
    }
    None
}

fn session_window(session: &Path) -> anyhow::Result<(Option<String>, Option<String>)> {
    let mut first = None;
    let mut last = None;
    for ev in iter_events(session)? {
        if let Some(ts) = ev.timestamp {
            if first.is_none() {
                first = Some(ts.clone());
            }
            last = Some(ts);
        }
    }
    Ok((first, last))
}

/// Bounded `git log`: the session's first/last events define the window, padded
/// by a day on each side so we don't miss amends/cherry-picks right at the
/// edges. Day-level precision is plenty — we re-score every commit against
/// the session anyway.
fn list_commits_in_window(
    repo: &Path,
    first: Option<&str>,
    last: Option<&str>,
) -> anyhow::Result<Vec<String>> {
    let mut args: Vec<String> = vec!["log".into(), "--format=%H".into()];
    if let Some(f) = first.and_then(day_shift(-1)) {
        args.push(format!("--since={f}"));
    }
    if let Some(l) = last.and_then(day_shift(1)) {
        args.push(format!("--until={l}"));
    }
    let out = Command::new("git").current_dir(repo).args(&args).output()?;
    if !out.status.success() {
        anyhow::bail!(
            "git log failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect())
}

/// Returns a closure `&str -> Option<String>` that shifts a `YYYY-MM-DD…`
/// timestamp by whole days. Done arithmetically on a Julian-day-ish counter
/// so we don't pull in chrono just for ±1-day bounds. Anything we can't
/// parse falls back to the calendar-day prefix (git accepts `YYYY-MM-DD`).
fn day_shift(delta_days: i64) -> impl Fn(&str) -> Option<String> {
    move |ts: &str| {
        let head: String = ts.chars().take(10).collect(); // YYYY-MM-DD
        let mut it = head.split('-');
        let y: i64 = it.next()?.parse().ok()?;
        let m: i64 = it.next()?.parse().ok()?;
        let d: i64 = it.next()?.parse().ok()?;
        let (ny, nm, nd) = add_days(y, m, d, delta_days);
        Some(format!("{ny:04}-{nm:02}-{nd:02}"))
    }
}

/// Add `delta` days to (y, m, d). Handles ±a few days across month/year
/// boundaries — all we need for log-window padding.
fn add_days(mut y: i64, mut m: i64, mut d: i64, delta: i64) -> (i64, i64, i64) {
    d += delta;
    while d < 1 {
        m -= 1;
        if m < 1 {
            m = 12;
            y -= 1;
        }
        d += days_in_month(y, m);
    }
    loop {
        let dim = days_in_month(y, m);
        if d <= dim {
            break;
        }
        d -= dim;
        m += 1;
        if m > 12 {
            m = 1;
            y += 1;
        }
    }
    (y, m, d)
}

fn days_in_month(y: i64, m: i64) -> i64 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
            if leap {
                29
            } else {
                28
            }
        }
        _ => 30,
    }
}

fn print_rows(id: &str, repo: &Path, rows: &[(Commit, Candidate)]) {
    println!(
        "{} {}  {}  {}",
        paint(DIM, "session"),
        paint(BOLD, id),
        paint(DIM, &format!("{} commit(s)", rows.len())),
        paint(DIM, &repo.display().to_string())
    );
    for (commit, cand) in rows {
        let time: String = commit.time.replace('T', " ").chars().take(19).collect();
        println!(
            "  {}  {}  {}  {}",
            paint(DIM, &commit.short_sha),
            paint(DIM, &time),
            tag_line(cand),
            commit.subject
        );
    }
}

fn tag_line(c: &Candidate) -> String {
    let mut parts = Vec::new();
    if c.authored_commit {
        parts.push(paint(GREEN, "authored"));
    }
    if c.matched_subject {
        parts.push(paint(GREEN, "subject"));
    }
    if c.matched_sha {
        parts.push(paint(GREEN, "sha"));
    }
    if c.window_covers_commit {
        parts.push(paint(YELLOW, "time-window"));
    }
    parts.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn climb_to_git_walks_up_to_dot_git() {
        let tmp = std::env::temp_dir().join(format!("cch-commits-climb-{}", std::process::id()));
        let sub = tmp.join("a/b/c");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::create_dir_all(tmp.join(".git")).unwrap();
        let got = climb_to_git(&sub.join("file.rs")).unwrap();
        assert_eq!(got.canonicalize().unwrap(), tmp.canonicalize().unwrap());
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn climb_to_git_returns_none_when_no_git_dir() {
        let tmp = std::env::temp_dir().join(format!("cch-commits-noclimb-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        assert!(climb_to_git(&tmp.join("x")).is_none());
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn add_days_crosses_month_and_year_boundaries() {
        assert_eq!(add_days(2026, 4, 24, 1), (2026, 4, 25));
        assert_eq!(add_days(2026, 4, 30, 1), (2026, 5, 1));
        assert_eq!(add_days(2026, 1, 1, -1), (2025, 12, 31));
        assert_eq!(add_days(2024, 2, 28, 1), (2024, 2, 29)); // leap year
        assert_eq!(add_days(2025, 2, 28, 1), (2025, 3, 1));
    }

    #[test]
    fn day_shift_handles_iso_timestamps() {
        let shift = day_shift(-1);
        assert_eq!(
            shift("2026-04-24T10:30:00.000Z").as_deref(),
            Some("2026-04-23")
        );
        assert_eq!(shift("garbage"), None);
    }
}
