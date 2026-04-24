//! `cch blame <sha>` — link a git commit back to the session that produced it.
//!
//! Cross-references three signals inside the session's project dir:
//!   1. the commit subject appearing verbatim (Claude usually writes it)
//!   2. the commit SHA (short or full) appearing in the transcript
//!   3. the commit time falling inside the session's activity window
//!
//! Signals are additive; the highest-scoring candidate wins.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, UNIX_EPOCH};

use crate::paths::{encode_cwd, projects_root};
use crate::session::first_user_prompt;
use crate::term::{paint, BOLD, CYAN, DIM, GREEN, YELLOW};
use crate::timebounds::format_systime_utc;
use crate::transcript::{iter_events, Event, Part};

pub struct Opts {
    /// Commit-ish or `A..B` range. Anything `git show` / `git log` accepts.
    pub sha: String,
}

pub struct Commit {
    pub full_sha: String,
    pub short_sha: String,
    /// UTC, normalized to `YYYY-MM-DDTHH:MM:SS`.
    pub time: String,
    pub subject: String,
    pub repo_root: PathBuf,
}

pub(crate) struct Candidate {
    pub(crate) path: PathBuf,
    first_prompt: Option<String>,
    first_ts: Option<String>,
    last_ts: Option<String>,
    pub(crate) matched_subject: bool,
    pub(crate) matched_sha: bool,
    pub(crate) window_covers_commit: bool,
    /// Session ran a `git commit` Bash tool_use that carries the subject or SHA
    /// in its argv — hard evidence this session *authored* the commit, not just
    /// mentioned it. Strongest signal; lets us drop "cited-only" candidates.
    pub(crate) authored_commit: bool,
}

impl Candidate {
    pub(crate) fn score(&self) -> u32 {
        (self.authored_commit as u32) * 200
            + (self.matched_subject as u32) * 100
            + (self.matched_sha as u32) * 50
            + (self.window_covers_commit as u32) * 10
    }

    /// A "cited only" candidate: the SHA appears in the transcript but nothing
    /// else links the session to the commit — typically a later session
    /// discussing the commit rather than producing it.
    fn is_cite_only(&self) -> bool {
        self.matched_sha
            && !self.authored_commit
            && !self.matched_subject
            && !self.window_covers_commit
    }
}

pub fn run(opts: Opts) -> anyhow::Result<i32> {
    if is_range(&opts.sha) {
        return run_range(&opts.sha);
    }
    let commit = load_commit(&opts.sha)?;
    let proj_dir = projects_root()?.join(encode_cwd(&commit.repo_root));

    if !proj_dir.is_dir() {
        eprintln!("No Claude Code sessions for {}", commit.repo_root.display());
        return Ok(1);
    }

    let (candidates, earliest) = rank_candidates(&proj_dir, &commit)?;

    // Authoring evidence = the session actually produced the commit, not just
    // mentioned its SHA or subject. A later session that runs `git log` or
    // discusses an old commit will fire `matched_subject`/`matched_sha` without
    // authorship; pairing subject-match with the session's time window covering
    // the commit filters those out. `authored_commit` (a `git commit` Bash
    // tool_use carrying the subject/SHA) is the hard signal and stands alone.
    let has_authoring_evidence = candidates
        .iter()
        .any(|c| c.authored_commit || (c.matched_subject && c.window_covers_commit));

    if !has_authoring_evidence {
        print_missing_author(&commit, &candidates, earliest.as_deref());
        return Ok(1);
    }

    print_result(&commit, &candidates);
    Ok(0)
}

fn is_range(s: &str) -> bool {
    // `git log` range syntax: `A..B` or `A...B`. Guard against `HEAD~2..`.
    s.contains("..")
}

fn run_range(range: &str) -> anyhow::Result<i32> {
    let out = Command::new("git")
        .args(["log", "--reverse", "--format=%H", range])
        .output()?;
    if !out.status.success() {
        anyhow::bail!(
            "git log failed for {range}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let shas: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();

    if shas.is_empty() {
        eprintln!("No commits in {range}.");
        return Ok(1);
    }

    let first = load_commit(&shas[0])?;
    let proj_dir = projects_root()?.join(encode_cwd(&first.repo_root));
    let have_dir = proj_dir.is_dir();

    let mut matched = 0usize;
    let mut last_sid: Option<String> = None;

    println!(
        "{} {}  {}",
        paint(DIM, "range"),
        paint(BOLD, range),
        paint(DIM, &format!("{} commit(s)", shas.len()))
    );

    for sha in &shas {
        let commit = load_commit(sha)?;
        let best = if have_dir {
            rank_candidates(&proj_dir, &commit)?.0.into_iter().next()
        } else {
            None
        };
        let short_id = best.as_ref().map(|c| {
            c.path
                .file_stem()
                .and_then(|s| s.to_str())
                .map(|s| s.get(..8).unwrap_or(s).to_string())
                .unwrap_or_default()
        });
        let sid_cell = match &short_id {
            Some(id) if last_sid.as_deref() == Some(id.as_str()) => paint(DIM, &" ".repeat(8)),
            Some(id) => paint(BOLD, id),
            None => paint(DIM, "--------"),
        };
        let tags = best
            .as_ref()
            .map(evidence_tags)
            .unwrap_or_else(|| paint(DIM, "no match"));
        println!(
            "  {}  {}  {}  {}",
            paint(DIM, &commit.short_sha),
            sid_cell,
            tags,
            commit.subject
        );
        if best.is_some() {
            matched += 1;
        }
        last_sid = short_id;
    }

    println!();
    println!(
        "{}",
        paint(
            DIM,
            &format!("{matched}/{} commit(s) mapped to a session.", shas.len())
        )
    );
    if matched == 0 {
        return Ok(1);
    }
    Ok(0)
}

pub(crate) fn rank_candidates(
    proj_dir: &Path,
    commit: &Commit,
) -> anyhow::Result<(Vec<Candidate>, Option<String>)> {
    let mut candidates = Vec::new();
    // Earliest session start across the whole project dir — used to tell
    // apart "no session produced this" from "the authoring session is
    // outside Claude Code's retention window".
    let mut earliest: Option<String> = None;
    for entry in std::fs::read_dir(proj_dir)?.filter_map(|e| e.ok()) {
        let p = entry.path();
        if p.extension().and_then(|s| s.to_str()) != Some("jsonl") {
            continue;
        }
        let c = scan(&p, commit)?;
        if let Some(ts) = c.first_ts.as_deref() {
            let ts_short = ts[..ts.len().min(19)].to_string();
            earliest = Some(match earliest {
                Some(e) if e <= ts_short => e,
                _ => ts_short,
            });
        }
        candidates.push(c);
    }
    candidates.retain(|c| c.score() > 0);
    // If any session actually authored the commit (ran `git commit` with the
    // subject/SHA), drop pure-citation sessions — they're just later chats
    // that happened to mention the SHA. Without this, a session that cites a
    // SHA can out-rank nothing but still clutters the top-3 display.
    let any_author = candidates.iter().any(|c| c.authored_commit);
    if any_author {
        candidates.retain(|c| !c.is_cite_only());
    }
    candidates.sort_by(|a, b| b.score().cmp(&a.score()).then(b.last_ts.cmp(&a.last_ts)));
    Ok((candidates, earliest))
}

pub(crate) fn load_commit(sha: &str) -> anyhow::Result<Commit> {
    load_commit_in(None, sha)
}

/// Same as [`load_commit`], but runs `git` inside an explicit repo instead of
/// the current working directory. Used by `cch commits <session>` to inspect a
/// repo we derived from the session's tool_use file paths.
pub(crate) fn load_commit_in(cwd: Option<&Path>, sha: &str) -> anyhow::Result<Commit> {
    let mut show = Command::new("git");
    if let Some(d) = cwd {
        show.current_dir(d);
    }
    let out = show
        .args(["show", "-s", "--format=%H%n%h%n%ct%n%s", sha])
        .output()?;
    if !out.status.success() {
        anyhow::bail!(
            "git show failed for {sha}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    let mut it = stdout.lines();
    let full_sha = it.next().unwrap_or("").trim().to_string();
    let short_sha = it.next().unwrap_or("").trim().to_string();
    let ct: u64 = it.next().unwrap_or("0").trim().parse().unwrap_or(0);
    let subject = it.next().unwrap_or("").trim().to_string();

    if full_sha.is_empty() {
        anyhow::bail!("empty commit SHA returned by git for {sha}");
    }

    let time = format_systime_utc(UNIX_EPOCH + Duration::from_secs(ct));

    let mut rev = Command::new("git");
    if let Some(d) = cwd {
        rev.current_dir(d);
    }
    let root_out = rev.args(["rev-parse", "--show-toplevel"]).output()?;
    if !root_out.status.success() {
        anyhow::bail!("not inside a git repository");
    }
    let root = String::from_utf8_lossy(&root_out.stdout).trim().to_string();
    let repo_root = PathBuf::from(&root).canonicalize()?;

    Ok(Commit {
        full_sha,
        short_sha,
        time,
        subject,
        repo_root,
    })
}

fn scan(path: &Path, commit: &Commit) -> anyhow::Result<Candidate> {
    scan_one(path, commit)
}

/// Evaluate a single transcript against a commit. Same signals as ranking —
/// exposed for `cch commits`, which needs per-session scoring without walking a
/// whole project dir.
pub(crate) fn scan_one(path: &Path, commit: &Commit) -> anyhow::Result<Candidate> {
    let mut body = evaluate(iter_events(path)?, commit);
    body.path = path.to_path_buf();
    body.first_prompt = first_user_prompt(path);
    Ok(body)
}

fn evaluate(events: impl Iterator<Item = Event>, commit: &Commit) -> Candidate {
    let mut first_ts: Option<String> = None;
    let mut last_ts: Option<String> = None;
    let mut matched_subject = false;
    let mut matched_sha = false;
    let mut authored_commit = false;

    let subject = commit.subject.as_str();
    let short = commit.short_sha.as_str();
    let full = commit.full_sha.as_str();

    for ev in events {
        if let Some(ts) = &ev.timestamp {
            if first_ts.is_none() {
                first_ts = Some(ts.clone());
            }
            last_ts = Some(ts.clone());
        }
        for part in &ev.parts {
            let text = match part {
                Part::Text(s) | Part::ToolResult(s) => s.as_str(),
                Part::ToolUse { summary, .. } => summary.as_str(),
            };
            if text.is_empty() {
                continue;
            }
            if !matched_subject && subject.len() >= 8 && text.contains(subject) {
                matched_subject = true;
            }
            if !matched_sha
                && ((short.len() >= 7 && text.contains(short))
                    || (full.len() >= 7 && text.contains(full)))
            {
                matched_sha = true;
            }
            // Authorship signal: a Bash tool_use whose argv contains both
            // `git commit` and either the subject or the full/short SHA.
            // The tool-input summary is trimmed to 120 chars, so for long
            // subjects we may miss a match — that's fine, matched_subject
            // via the plain `commit.subject` contains() still fires elsewhere.
            if !authored_commit {
                if let Part::ToolUse { name, summary, .. } = part {
                    if name == "Bash"
                        && summary.contains("git commit")
                        && ((subject.len() >= 8 && summary.contains(subject))
                            || (short.len() >= 7 && summary.contains(short))
                            || (full.len() >= 7 && summary.contains(full)))
                    {
                        authored_commit = true;
                    }
                }
            }
        }
    }

    let window_covers_commit = match (first_ts.as_deref(), last_ts.as_deref()) {
        (Some(a), Some(b)) => {
            let a = &a[..a.len().min(19)];
            let b = &b[..b.len().min(19)];
            let c = commit.time.as_str();
            a <= c && c <= b
        }
        _ => false,
    };

    Candidate {
        path: PathBuf::new(),
        first_prompt: None,
        first_ts,
        last_ts,
        matched_subject,
        matched_sha,
        window_covers_commit,
        authored_commit,
    }
}

fn print_result(commit: &Commit, candidates: &[Candidate]) {
    let time = commit.time.replace('T', " ");
    println!(
        "{} {} {}",
        paint(DIM, "commit"),
        paint(BOLD, &commit.short_sha),
        paint(DIM, &time)
    );
    if !commit.subject.is_empty() {
        println!("  {}", commit.subject);
    }
    println!();

    for (i, c) in candidates.iter().take(3).enumerate() {
        let id = c.path.file_stem().and_then(|s| s.to_str()).unwrap_or("?");
        let short_id = id.get(..8).unwrap_or(id);
        let tags = evidence_tags(c);
        println!(
            "{}  {}  {}",
            paint(BOLD, short_id),
            paint(CYAN, &tags),
            paint(DIM, &fmt_window(c))
        );
        if let Some(p) = &c.first_prompt {
            let flat: String = p.split_whitespace().collect::<Vec<_>>().join(" ");
            let preview: String = flat.chars().take(110).collect();
            let ell = if flat.chars().count() > 110 {
                "…"
            } else {
                ""
            };
            println!("  {}{}", paint(DIM, &preview), paint(DIM, ell));
        }
        if i == 0 {
            println!("  {}", paint(DIM, &format!("→ cch show {short_id}")));
        }
    }
}

/// Rendered when no retained session has authoring evidence for the commit.
/// Distinguishes "authoring session was deleted" from "the SHA gets mentioned
/// here later" — previously these collapsed into misleading cite-only matches.
fn print_missing_author(commit: &Commit, candidates: &[Candidate], earliest: Option<&str>) {
    let time = commit.time.replace('T', " ");
    eprintln!(
        "{} {} {}",
        paint(DIM, "commit"),
        paint(BOLD, &commit.short_sha),
        paint(DIM, &time),
    );
    if !commit.subject.is_empty() {
        eprintln!("  {}", commit.subject);
    }
    eprintln!();

    let commit_time = &commit.time[..commit.time.len().min(19)];
    let predates = earliest.map(|e| commit_time < e).unwrap_or(false);

    let msg = if predates {
        "No session produced this commit — it predates the oldest retained session for this project (likely outside Claude Code's retention window)."
    } else if earliest.is_some() {
        "No session produced this commit — the authoring session is no longer retained."
    } else {
        "No session produced this commit."
    };
    eprintln!("{}", paint(YELLOW, msg));

    let cites: Vec<&Candidate> = candidates.iter().filter(|c| c.matched_sha).collect();
    if !cites.is_empty() {
        eprintln!();
        eprintln!(
            "{}",
            paint(
                DIM,
                &format!("SHA mentioned in {} later session(s):", cites.len()),
            ),
        );
        for c in cites.iter().take(3) {
            let id = c.path.file_stem().and_then(|s| s.to_str()).unwrap_or("?");
            let short_id = id.get(..8).unwrap_or(id);
            eprintln!("  {}  {}", paint(DIM, short_id), paint(DIM, &fmt_window(c)),);
        }
    }
}

fn evidence_tags(c: &Candidate) -> String {
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

fn fmt_window(c: &Candidate) -> String {
    let short = |s: &Option<String>| {
        s.as_deref()
            .map(|t| t[..t.len().min(19)].replace('T', " "))
            .unwrap_or_default()
    };
    let a = short(&c.first_ts);
    let b = short(&c.last_ts);
    if a.is_empty() && b.is_empty() {
        String::new()
    } else {
        format!("{a} → {b}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transcript::parse_event;

    fn events_from(lines: &[&str]) -> Vec<Event> {
        lines.iter().filter_map(|l| parse_event(l)).collect()
    }

    fn commit(subject: &str, short: &str, full: &str, time: &str) -> Commit {
        Commit {
            full_sha: full.into(),
            short_sha: short.into(),
            time: time.into(),
            subject: subject.into(),
            repo_root: PathBuf::from("/tmp"),
        }
    }

    #[test]
    fn subject_match_is_strongest_signal() {
        let evs = events_from(&[
            r#"{"type":"assistant","message":{"content":"Let me commit: Add fancy feature"}}"#,
        ]);
        let c = evaluate(
            evs.into_iter(),
            &commit(
                "Add fancy feature",
                "abcdef1",
                "abcdef1234",
                "2026-04-23T10:00:00",
            ),
        );
        assert!(c.matched_subject);
        assert!(!c.matched_sha);
        assert!(c.score() >= 100);
    }

    #[test]
    fn sha_match_detected() {
        let evs = events_from(&[
            r#"{"type":"user","message":{"content":"look at commit 5e069c9 please"}}"#,
        ]);
        let c = evaluate(
            evs.into_iter(),
            &commit("X", "5e069c9", "5e069c9abc123", "2026-04-23T10:00:00"),
        );
        assert!(c.matched_sha);
    }

    #[test]
    fn time_window_covers_commit() {
        let evs = events_from(&[
            r#"{"type":"user","timestamp":"2026-04-23T09:00:00.000Z","message":{"content":"start"}}"#,
            r#"{"type":"assistant","timestamp":"2026-04-23T11:00:00.000Z","message":{"content":"end"}}"#,
        ]);
        let c = evaluate(
            evs.into_iter(),
            &commit(
                "unrelated subject not here",
                "x",
                "y",
                "2026-04-23T10:00:00",
            ),
        );
        assert!(c.window_covers_commit);
        assert!(!c.matched_subject);
        assert_eq!(c.score(), 10);
    }

    #[test]
    fn time_window_excludes_commit_outside_range() {
        let evs = events_from(&[
            r#"{"type":"user","timestamp":"2026-04-22T09:00:00.000Z","message":{"content":"x"}}"#,
            r#"{"type":"assistant","timestamp":"2026-04-22T10:00:00.000Z","message":{"content":"y"}}"#,
        ]);
        let c = evaluate(
            evs.into_iter(),
            &commit("s", "x", "y", "2026-04-23T10:00:00"),
        );
        assert!(!c.window_covers_commit);
    }

    #[test]
    fn short_subject_not_matched_to_avoid_false_positives() {
        // Subject too short (< 8 chars) is skipped — would false-positive everywhere.
        let evs =
            events_from(&[r#"{"type":"user","message":{"content":"anywhere here fix goes"}}"#]);
        let c = evaluate(
            evs.into_iter(),
            &commit("fix", "abcdef1", "abcdef1234", "2026-04-23T10:00:00"),
        );
        assert!(!c.matched_subject);
    }

    #[test]
    fn score_additive() {
        let evs = events_from(&[
            r#"{"type":"user","timestamp":"2026-04-23T09:00:00.000Z","message":{"content":"start"}}"#,
            r#"{"type":"assistant","timestamp":"2026-04-23T10:30:00.000Z","message":{"content":"commit message: Nice feature added"}}"#,
            r#"{"type":"assistant","timestamp":"2026-04-23T11:00:00.000Z","message":{"content":"sha is deadbeef123"}}"#,
        ]);
        let c = evaluate(
            evs.into_iter(),
            &commit(
                "Nice feature added",
                "deadbee",
                "deadbeef123",
                "2026-04-23T10:30:00",
            ),
        );
        assert!(c.matched_subject);
        assert!(c.matched_sha);
        assert!(c.window_covers_commit);
        assert_eq!(c.score(), 160);
    }

    #[test]
    fn searches_tool_use_summary() {
        // `git commit -m "..."` shows up as a tool_use whose summary contains the subject.
        let evs = events_from(&[
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Bash","input":{"command":"git commit -m 'Refactor parser module'"}}]}}"#,
        ]);
        let c = evaluate(
            evs.into_iter(),
            &commit(
                "Refactor parser module",
                "abcdef1",
                "abcdef1234567",
                "2026-04-23T10:00:00",
            ),
        );
        assert!(c.matched_subject);
    }

    #[test]
    fn range_detection() {
        assert!(is_range("main..HEAD"));
        assert!(is_range("v1.2..v1.3"));
        assert!(is_range("HEAD~3..HEAD"));
        assert!(is_range("A...B"));
        assert!(!is_range("HEAD"));
        assert!(!is_range("5e069c9"));
        assert!(!is_range("HEAD~3"));
    }

    #[test]
    fn authored_when_bash_git_commit_carries_subject() {
        let evs = events_from(&[
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Bash","input":{"command":"git commit -m 'Refactor parser module'"}}]}}"#,
        ]);
        let c = evaluate(
            evs.into_iter(),
            &commit(
                "Refactor parser module",
                "abcdef1",
                "abcdef1234567",
                "2026-04-23T10:00:00",
            ),
        );
        assert!(c.authored_commit);
        // Authoring dominates any citation score.
        assert!(c.score() >= 200);
    }

    #[test]
    fn authored_requires_git_commit_in_argv_not_just_sha_in_text() {
        // Bare SHA mentioned in a message must NOT set authored_commit —
        // that's citation, not authorship.
        let evs =
            events_from(&[r#"{"type":"user","message":{"content":"look at abcdef1 please"}}"#]);
        let c = evaluate(
            evs.into_iter(),
            &commit(
                "Unrelated subject here",
                "abcdef1",
                "abcdef1234567",
                "2026-04-23T10:00:00",
            ),
        );
        assert!(!c.authored_commit);
        assert!(c.matched_sha);
        assert!(c.is_cite_only());
    }

    #[test]
    fn authored_also_triggers_on_sha_in_git_commit_argv() {
        // Rare but real: `git commit -C <sha>` / amend flows — argv carries
        // the SHA rather than the subject. Still authorship.
        let evs = events_from(&[
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Bash","input":{"command":"git commit --amend -C deadbeef"}}]}}"#,
        ]);
        let c = evaluate(
            evs.into_iter(),
            &commit(
                "Anything here",
                "deadbee",
                "deadbeef12345",
                "2026-04-23T10:00:00",
            ),
        );
        assert!(c.authored_commit);
    }

    #[test]
    fn no_match_when_unrelated() {
        let evs = events_from(&[
            r#"{"type":"user","timestamp":"2026-04-22T09:00:00.000Z","message":{"content":"nothing to see"}}"#,
        ]);
        let c = evaluate(
            evs.into_iter(),
            &commit(
                "Completely different thing",
                "ffffff1",
                "fffff12345",
                "2026-04-23T10:00:00",
            ),
        );
        assert_eq!(c.score(), 0);
    }
}
