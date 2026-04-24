//! End-to-end tests that invoke the compiled `cch` binary.
//!
//! Each test builds a fake Claude Code tree under a unique temp `$HOME`, then
//! runs `cch` with that HOME set. Stdin/stdout is captured. We disable colour
//! (`NO_COLOR=1`) for deterministic assertions.
//!
//! Why subprocesses instead of calling `cli::dispatch` directly? Two of the
//! commands (`session`, `grep --here`) read `std::env::current_dir()` and the
//! result depends on the process-wide CWD — hard to control cleanly from
//! inside a cargo test runner that parallelises. A child process gives each
//! test its own CWD and HOME.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_cch"))
}

/// Build an isolated HOME under /tmp and return its canonical path.
fn fresh_home(tag: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("cch-it-{tag}-{}-{}", std::process::id(), n));
    std::fs::create_dir_all(&dir).unwrap();
    // Canonicalize because `/tmp` itself may be a symlink on some systems and
    // the binary canonicalizes the cwd before encoding it.
    dir.canonicalize().unwrap()
}

/// Encode an absolute path the way Claude Code does: '/' and '.' → '-'.
fn encode_cwd(p: &Path) -> String {
    p.to_string_lossy()
        .chars()
        .map(|c| if c == '/' || c == '.' { '-' } else { c })
        .collect()
}

fn projects_dir_for(home: &Path, cwd: &Path) -> PathBuf {
    home.join(".claude").join("projects").join(encode_cwd(cwd))
}

fn write_jsonl(dir: &Path, uuid: &str, lines: &[&str]) -> PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    let path = dir.join(format!("{uuid}.jsonl"));
    let mut f = std::fs::File::create(&path).unwrap();
    for l in lines {
        writeln!(f, "{l}").unwrap();
    }
    path
}

/// Run `cch ARGS` with HOME overridden and an explicit working directory.
fn run(home: &Path, cwd: &Path, args: &[&str]) -> Output {
    Command::new(bin())
        .args(args)
        .env("HOME", home)
        .env("NO_COLOR", "1")
        .env_remove("CLICOLOR_FORCE")
        .current_dir(cwd)
        .output()
        .expect("spawn cch")
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}
fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

/// Force a file's mtime to an absolute Unix timestamp. Uses `touch -d @<secs>`
/// — std doesn't expose utimensat and we don't want a build-time dep just for
/// tests.
fn set_mtime_secs(path: &Path, secs: i64) {
    let status = Command::new("touch")
        .args(["-d", &format!("@{secs}")])
        .arg(path)
        .status()
        .expect("spawn touch");
    assert!(status.success(), "touch failed for {}", path.display());
}

// --- help / version ----------------------------------------------------------

#[test]
fn help_mentions_each_subcommand() {
    let home = fresh_home("help");
    let out = run(&home, &home, &["--help"]);
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    let s = stdout(&out);
    assert!(s.contains("session"));
    assert!(s.contains("grep"));
    assert!(s.contains("show"));
}

#[test]
fn version_prints_package_version() {
    let home = fresh_home("version");
    let out = run(&home, &home, &["--version"]);
    assert!(out.status.success());
    assert!(stdout(&out).contains("cch"));
}

#[test]
fn bare_call_shows_help_and_errors() {
    // `arg_required_else_help = true` → clap exits non-zero with help text on stderr.
    let home = fresh_home("bare");
    let out = run(&home, &home, &[]);
    assert!(!out.status.success());
    let all = format!("{}{}", stdout(&out), stderr(&out));
    assert!(all.contains("session") && all.contains("grep"));
}

// --- session -----------------------------------------------------------------

#[test]
fn session_lists_most_recent_first() {
    let home = fresh_home("session-list");
    let cwd = home.join("proj");
    std::fs::create_dir_all(&cwd).unwrap();
    let cwd = cwd.canonicalize().unwrap();
    let pdir = projects_dir_for(&home, &cwd);

    write_jsonl(
        &pdir,
        "aaaaaaa1-0000-0000-0000-000000000000",
        &[r#"{"type":"user","message":{"content":"first session prompt"}}"#],
    );
    std::thread::sleep(std::time::Duration::from_millis(15));
    write_jsonl(
        &pdir,
        "bbbbbbb2-0000-0000-0000-000000000000",
        &[r#"{"type":"user","message":{"content":"latest session prompt"}}"#],
    );

    let out = run(&home, &cwd, &["session"]);
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    let s = stdout(&out);
    // Both should appear and the newer should be -1 (top).
    let top = s.lines().next().unwrap_or_default();
    assert!(top.contains("-1 "), "top line: {top:?}");
    assert!(
        top.contains("bbbbbbb2"),
        "top line missing newer id: {top:?}"
    );
    assert!(s.contains("aaaaaaa1"));
    assert!(s.contains("latest session prompt"));
}

#[test]
fn session_n_shortcut_limits_and_expands() {
    let home = fresh_home("session-n");
    let cwd = home.join("proj");
    std::fs::create_dir_all(&cwd).unwrap();
    let cwd = cwd.canonicalize().unwrap();
    let pdir = projects_dir_for(&home, &cwd);

    for i in 0..3 {
        write_jsonl(
            &pdir,
            &format!("uuid000{i}-0000-0000-0000-000000000000"),
            &[&format!(
                r#"{{"type":"user","message":{{"content":"prompt {i}"}}}}"#
            )],
        );
        std::thread::sleep(std::time::Duration::from_millis(15));
    }

    // `-2` is the head-style shortcut for `-n 2`.
    let out = run(&home, &cwd, &["session", "-2"]);
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    let s = stdout(&out);
    // Full-form output includes "Les 2 sessions les plus récentes." header.
    assert!(s.contains("Les 2 sessions"), "output: {s}");
    // Two bullets [-1] and [-2], no [-3].
    assert!(s.contains("[-1]"));
    assert!(s.contains("[-2]"));
    assert!(!s.contains("[-3]"));
}

#[test]
fn session_no_sessions_exits_1() {
    let home = fresh_home("session-empty");
    // CWD has no corresponding project directory.
    let cwd = home.join("nowhere");
    std::fs::create_dir_all(&cwd).unwrap();
    let cwd = cwd.canonicalize().unwrap();

    let out = run(&home, &cwd, &["session"]);
    assert!(!out.status.success());
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr(&out).to_lowercase().contains("no sessions"));
}

#[test]
fn session_touched_no_match_message_is_not_window() {
    // Regression: when `--touched` matched nothing, stderr reused the date
    // filter's wording ("No sessions in that window …"), which is confusing
    // — nothing about `--touched` is a time window. The message must
    // reflect the active filter so users know what actually excluded hits.
    let home = fresh_home("session-touched-miss");
    let cwd = home.join("proj");
    std::fs::create_dir_all(&cwd).unwrap();
    let cwd = cwd.canonicalize().unwrap();
    let pdir = projects_dir_for(&home, &cwd);
    // One valid session that edits an unrelated file.
    write_jsonl(
        &pdir,
        "touchmis-0000-0000-0000-000000000000",
        &[
            r#"{"type":"user","message":{"content":"hi"}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Edit","input":{"file_path":"/repo/other.rs"}}]}}"#,
        ],
    );

    let out = run(&home, &cwd, &["session", "--touched", "nope.rs"]);
    assert!(!out.status.success());
    let err = stderr(&out).to_lowercase();
    // Must signal this was a `--touched` miss, not a date-window miss.
    assert!(
        !err.contains("window"),
        "stderr leaks date-filter wording for --touched miss: {err}"
    );
    assert!(
        err.contains("touched") || err.contains("nope.rs"),
        "stderr should reference --touched or the queried path: {err}"
    );
}

#[test]
fn session_filters_empty_prompts_by_default() {
    let home = fresh_home("session-filter");
    let cwd = home.join("proj");
    std::fs::create_dir_all(&cwd).unwrap();
    let cwd = cwd.canonicalize().unwrap();
    let pdir = projects_dir_for(&home, &cwd);

    // Session with only a wrapper — should be hidden without --all.
    write_jsonl(
        &pdir,
        "wrapper1-0000-0000-0000-000000000000",
        &[r#"{"type":"user","message":{"content":"<system-reminder>x</system-reminder>"}}"#],
    );
    // Real session.
    write_jsonl(
        &pdir,
        "realone1-0000-0000-0000-000000000000",
        &[r#"{"type":"user","message":{"content":"actual"}}"#],
    );

    let out = run(&home, &cwd, &["session"]);
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    let s = stdout(&out);
    assert!(s.contains("realone1"));
    assert!(!s.contains("wrapper1"), "wrapper leaked: {s}");

    // --all shows both.
    let out = run(&home, &cwd, &["session", "--all"]);
    assert!(out.status.success());
    let s = stdout(&out);
    assert!(s.contains("realone1"));
    assert!(s.contains("wrapper1"));
}

#[test]
fn session_default_caps_at_ten_and_prints_footer() {
    let home = fresh_home("session-cap");
    let cwd = home.join("proj");
    std::fs::create_dir_all(&cwd).unwrap();
    let cwd = cwd.canonicalize().unwrap();
    let pdir = projects_dir_for(&home, &cwd);

    // 12 sessions > the 10-line default cap.
    for i in 0..12 {
        write_jsonl(
            &pdir,
            &format!("capuuid{i:02}-0000-0000-0000-000000000000"),
            &[&format!(
                r#"{{"type":"user","message":{{"content":"p{i}"}}}}"#
            )],
        );
    }

    let out = run(&home, &cwd, &["session"]);
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    let s = stdout(&out);
    // Exactly 10 `-N ` list markers (one per line) plus a footer.
    let list_lines = s.lines().filter(|l| l.starts_with('-')).count();
    assert_eq!(list_lines, 10, "got {list_lines} list lines: {s}");
    assert!(s.contains("2 older"), "expected truncation footer: {s}");
    assert!(
        s.contains("cch session -n 12"),
        "footer should hint full count: {s}"
    );

    // With -n 12 the footer goes away (expanded view) and all 12 show.
    let out = run(&home, &cwd, &["session", "-n", "12"]);
    assert!(out.status.success());
    let s = stdout(&out);
    assert!(s.contains("Les 12 sessions"));
    assert!(!s.contains("older —"));
}

#[test]
fn session_time_bounds_select_window() {
    // Filter on mtime. We drive the mtime directly via filetime-free touch:
    // write files sequentially and pin their mtimes with `utimensat` via std.
    let home = fresh_home("session-time");
    let cwd = home.join("proj");
    std::fs::create_dir_all(&cwd).unwrap();
    let cwd = cwd.canonicalize().unwrap();
    let pdir = projects_dir_for(&home, &cwd);

    let old = write_jsonl(
        &pdir,
        "old00000-0000-0000-0000-000000000000",
        &[r#"{"type":"user","message":{"content":"ancient"}}"#],
    );
    let recent = write_jsonl(
        &pdir,
        "new00000-0000-0000-0000-000000000000",
        &[r#"{"type":"user","message":{"content":"recent"}}"#],
    );

    // Set explicit mtimes: old = 2026-04-22, recent = 2026-04-23T12:00.
    set_mtime_secs(&old, 1_776_816_000); // 2026-04-22T00:00:00 UTC
    set_mtime_secs(&recent, 1_776_945_600); // 2026-04-23T12:00:00 UTC

    // `--after 2026-04-23` keeps only the recent one.
    let out = run(&home, &cwd, &["session", "--after", "2026-04-23"]);
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    let s = stdout(&out);
    assert!(s.contains("new00000"));
    assert!(!s.contains("old00000"), "old leaked: {s}");

    // Window that excludes both → exit 1.
    let out = run(
        &home,
        &cwd,
        &["session", "--after", "2026-04-24", "--before", "2026-04-25"],
    );
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr(&out).to_lowercase().contains("window"));

    // Bad bound is reported.
    let out = run(&home, &cwd, &["session", "--after", "not-a-date"]);
    assert!(!out.status.success());
    assert!(stderr(&out).to_lowercase().contains("date") || stderr(&out).contains("invalid"));
}

#[test]
fn session_rejects_zero_count() {
    let home = fresh_home("session-zero");
    let cwd = home.join("proj");
    std::fs::create_dir_all(&cwd).unwrap();
    let cwd = cwd.canonicalize().unwrap();
    let pdir = projects_dir_for(&home, &cwd);
    write_jsonl(
        &pdir,
        "uuid-ok",
        &[r#"{"type":"user","message":{"content":"x"}}"#],
    );

    let out = run(&home, &cwd, &["session", "-n", "0"]);
    assert!(!out.status.success());
    assert_eq!(out.status.code(), Some(2));
}

// --- grep --------------------------------------------------------------------

#[test]
fn grep_finds_match_across_projects() {
    let home = fresh_home("grep-all");

    // Two different project dirs, both with a transcript.
    let p1 = home.join(".claude/projects/-fake-one");
    let p2 = home.join(".claude/projects/-fake-two");
    write_jsonl(
        &p1,
        "aa000001-0000-0000-0000-000000000000",
        &[r#"{"type":"assistant","message":{"content":"MAGIC WORD here"}}"#],
    );
    write_jsonl(
        &p2,
        "bb000002-0000-0000-0000-000000000000",
        &[r#"{"type":"assistant","message":{"content":"no hit"}}"#],
    );

    let out = run(&home, &home, &["grep", "MAGIC"]);
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    let s = stdout(&out);
    assert!(s.contains("aa000001"));
    assert!(!s.contains("bb000002"));
    assert!(s.contains("MAGIC"));
}

#[test]
fn grep_case_insensitive_by_default_and_sensitive_flag_narrows() {
    let home = fresh_home("grep-ci");
    let p = home.join(".claude/projects/-proj");
    write_jsonl(
        &p,
        "cccccccc-0000-0000-0000-000000000000",
        &[r#"{"type":"assistant","message":{"content":"The MeMChR library is fast"}}"#],
    );

    let out = run(&home, &home, &["grep", "memchr"]);
    assert!(out.status.success(), "ci miss: {}", stderr(&out));
    assert!(stdout(&out).contains("cccccccc"));

    let out = run(&home, &home, &["grep", "memchr", "-s"]);
    assert_eq!(out.status.code(), Some(1), "expected no match with -s");
    assert!(stdout(&out).is_empty());
}

#[test]
fn grep_time_bounds_select_window() {
    let home = fresh_home("grep-time");
    let p = home.join(".claude/projects/-proj");
    write_jsonl(
        &p,
        "time0000-0000-0000-0000-000000000000",
        &[
            r#"{"type":"user","timestamp":"2026-04-22T12:00:00.000Z","message":{"content":"DATETAG early"}}"#,
            r#"{"type":"user","timestamp":"2026-04-23T16:30:00.000Z","message":{"content":"DATETAG middle"}}"#,
            r#"{"type":"user","timestamp":"2026-04-23T18:00:00.000Z","message":{"content":"DATETAG late"}}"#,
        ],
    );

    let out = run(
        &home,
        &home,
        &[
            "grep",
            "DATETAG",
            "--after",
            "2026-04-23",
            "--before",
            "2026-04-23T17:00",
        ],
    );
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    let s = stdout(&out);
    // Filter to actual role-tagged hit lines (the preview repeats the
    // session's first prompt regardless of filters).
    let hits: Vec<_> = s
        .lines()
        .filter(|l| l.trim_start().starts_with("user") && l.contains("DATETAG"))
        .collect();
    assert_eq!(hits.len(), 1, "want one hit, got: {s}");
    assert!(hits[0].contains("middle"), "wrong hit: {s}");
}

#[test]
fn grep_invalid_bound_errors_out() {
    let home = fresh_home("grep-bad-time");
    let p = home.join(".claude/projects/-proj");
    write_jsonl(
        &p,
        "badt0000-0000-0000-0000-000000000000",
        &[r#"{"type":"user","message":{"content":"whatever"}}"#],
    );
    let out = run(
        &home,
        &home,
        &["grep", "whatever", "--before", "not-a-date"],
    );
    assert!(!out.status.success());
    assert!(stderr(&out).to_lowercase().contains("date") || stderr(&out).contains("invalid"));
}

#[test]
fn grep_no_match_exits_1() {
    let home = fresh_home("grep-0");
    let p = home.join(".claude/projects/-proj");
    write_jsonl(
        &p,
        "nope0000-0000-0000-0000-000000000000",
        &[r#"{"type":"assistant","message":{"content":"unrelated"}}"#],
    );
    let out = run(&home, &home, &["grep", "absent-needle"]);
    assert_eq!(out.status.code(), Some(1));
}

#[test]
fn grep_role_filter_restricts_hits() {
    let home = fresh_home("grep-role");
    let p = home.join(".claude/projects/-proj");
    write_jsonl(
        &p,
        "roles000-0000-0000-0000-000000000000",
        &[
            r#"{"type":"user","message":{"content":"ZEEK in user"}}"#,
            r#"{"type":"assistant","message":{"content":"ZEEK in assistant"}}"#,
        ],
    );

    let out = run(&home, &home, &["grep", "ZEEK", "--role", "user"]);
    assert!(out.status.success());
    let s = stdout(&out);
    // Hit lines are role-tagged; the preview/first-prompt line isn't tagged
    // but may echo the needle, so we filter by the role tag.
    let hit_lines: Vec<_> = s
        .lines()
        .filter(|l| l.trim_start().starts_with("user") && l.contains("ZEEK"))
        .collect();
    assert_eq!(hit_lines.len(), 1, "output: {s}");
    // And no `assistant`-tagged hit line.
    assert!(
        !s.lines()
            .any(|l| l.trim_start().starts_with("assistant") && l.contains("ZEEK")),
        "assistant leaked: {s}"
    );
}

#[test]
fn grep_here_restricts_to_current_project() {
    let home = fresh_home("grep-here");
    let cwd = home.join("proj-a");
    std::fs::create_dir_all(&cwd).unwrap();
    let cwd = cwd.canonicalize().unwrap();

    let pdir_here = projects_dir_for(&home, &cwd);
    let pdir_other = home.join(".claude/projects/-other-proj");
    write_jsonl(
        &pdir_here,
        "here0000-0000-0000-0000-000000000000",
        &[r#"{"type":"assistant","message":{"content":"SHARED term"}}"#],
    );
    write_jsonl(
        &pdir_other,
        "other000-0000-0000-0000-000000000000",
        &[r#"{"type":"assistant","message":{"content":"SHARED term"}}"#],
    );

    let out = run(&home, &cwd, &["grep", "SHARED", "--here"]);
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    let s = stdout(&out);
    assert!(s.contains("here0000"));
    assert!(!s.contains("other000"));
}

#[test]
fn grep_skips_sidechain_unless_requested() {
    let home = fresh_home("grep-sc");
    let p = home.join(".claude/projects/-proj");
    write_jsonl(
        &p,
        "sc000000-0000-0000-0000-000000000000",
        &[
            r#"{"type":"assistant","isSidechain":true,"message":{"content":"HIDDEN-ONLY in sidechain"}}"#,
        ],
    );
    let out = run(&home, &home, &["grep", "HIDDEN-ONLY"]);
    assert_eq!(out.status.code(), Some(1));

    let out = run(&home, &home, &["grep", "HIDDEN-ONLY", "--sidechains"]);
    assert!(out.status.success());
    assert!(stdout(&out).contains("sc000000"));
}

// --- show --------------------------------------------------------------------

#[test]
fn show_resolves_by_prefix_and_renders_content() {
    let home = fresh_home("show");
    let p = home.join(".claude/projects/-my-proj");
    write_jsonl(
        &p,
        "abcd1234-0000-0000-0000-000000000000",
        &[
            r#"{"type":"user","message":{"content":"first user question"}}"#,
            r#"{"type":"assistant","message":{"content":"assistant response"}}"#,
        ],
    );

    let out = run(&home, &home, &["show", "abcd"]);
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    let s = stdout(&out);
    assert!(s.contains("abcd1234"));
    assert!(s.contains("first user question"));
    assert!(s.contains("assistant response"));
    assert!(s.contains("#1 user"));
    assert!(s.contains("#2 assistant"));
}

#[test]
fn show_missing_prefix_fails() {
    let home = fresh_home("show-404");
    std::fs::create_dir_all(home.join(".claude/projects")).unwrap();
    let out = run(&home, &home, &["show", "doesnotexist"]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("no session"));
}

#[test]
fn show_ambiguous_prefix_fails_and_lists_candidates() {
    let home = fresh_home("show-amb");
    let p1 = home.join(".claude/projects/-one");
    let p2 = home.join(".claude/projects/-two");
    write_jsonl(
        &p1,
        "abcd1111-0000-0000-0000-000000000000",
        &[r#"{"type":"user","message":{"content":"a"}}"#],
    );
    write_jsonl(
        &p2,
        "abcd2222-0000-0000-0000-000000000000",
        &[r#"{"type":"user","message":{"content":"b"}}"#],
    );
    let out = run(&home, &home, &["show", "abcd"]);
    assert!(!out.status.success());
    let err = stderr(&out);
    assert!(err.contains("ambiguous"));
    assert!(err.contains("abcd1111"));
    assert!(err.contains("abcd2222"));
}

#[test]
fn show_hides_system_events_by_default_and_includes_with_flag() {
    let home = fresh_home("show-sys");
    let p = home.join(".claude/projects/-p");
    write_jsonl(
        &p,
        "sysabcd1-0000-0000-0000-000000000000",
        &[
            r#"{"type":"system","message":{"content":"SYS-SECRET"}}"#,
            r#"{"type":"user","message":{"content":"hi"}}"#,
        ],
    );

    let out = run(&home, &home, &["show", "sysabcd1"]);
    assert!(out.status.success());
    assert!(!stdout(&out).contains("SYS-SECRET"));

    let out = run(&home, &home, &["show", "sysabcd1", "--system"]);
    assert!(out.status.success());
    assert!(stdout(&out).contains("SYS-SECRET"));
}

#[test]
fn show_hides_sidechain_by_default() {
    let home = fresh_home("show-sc");
    let p = home.join(".claude/projects/-p");
    write_jsonl(
        &p,
        "scabcd11-0000-0000-0000-000000000000",
        &[
            r#"{"type":"user","isSidechain":true,"message":{"content":"SUB-AGENT-ONLY"}}"#,
            r#"{"type":"user","message":{"content":"main"}}"#,
        ],
    );
    let out = run(&home, &home, &["show", "scabcd11"]);
    assert!(out.status.success());
    assert!(!stdout(&out).contains("SUB-AGENT-ONLY"));

    let out = run(&home, &home, &["show", "scabcd11", "--sidechains"]);
    assert!(out.status.success());
    assert!(stdout(&out).contains("SUB-AGENT-ONLY"));
}

#[test]
fn show_renders_tool_use_and_tool_result() {
    let home = fresh_home("show-tools");
    let p = home.join(".claude/projects/-p");
    write_jsonl(
        &p,
        "toolsa01-0000-0000-0000-000000000000",
        &[
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"running"},{"type":"tool_use","name":"Bash","input":{"command":"ls -la"}}]}}"#,
            r#"{"type":"user","message":{"content":[{"type":"tool_result","content":[{"type":"text","text":"TOOL-OUTPUT"}]}]}}"#,
        ],
    );
    let out = run(&home, &home, &["show", "toolsa01"]);
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    let s = stdout(&out);
    assert!(s.contains("[tool_use] Bash"), "stdout was: {s}");
    assert!(s.contains("command=ls -la"));
    assert!(s.contains("TOOL-OUTPUT"));
    assert!(s.contains("#2 tool")); // relabelled event header with turn number
}

// --- show --turns ------------------------------------------------------------

fn write_multiturn(home: &std::path::Path, id: &str) {
    let p = home.join(".claude/projects/-multi");
    write_jsonl(
        &p,
        id,
        &[
            r#"{"type":"user","message":{"content":"Q1"}}"#,
            r#"{"type":"assistant","message":{"content":"A1"}}"#,
            r#"{"type":"user","message":{"content":"Q2"}}"#,
            r#"{"type":"assistant","message":{"content":"A2"}}"#,
            r#"{"type":"user","message":{"content":"Q3"}}"#,
            r#"{"type":"assistant","message":{"content":"A3"}}"#,
        ],
    );
}

#[test]
fn show_default_footer_shows_total_turns() {
    let home = fresh_home("show-footer");
    write_multiturn(&home, "ftr00001-0000-0000-0000-000000000000");
    let out = run(&home, &home, &["show", "ftr00001"]);
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    let s = stdout(&out);
    assert!(s.contains("6 turns"), "footer missing total. stdout:\n{s}");
    // All 6 turn markers present
    for i in 1..=6 {
        assert!(s.contains(&format!("#{i} ")), "missing #{i} in:\n{s}");
    }
}

#[test]
fn show_turns_head_keeps_only_first_n() {
    let home = fresh_home("show-head");
    write_multiturn(&home, "head0001-0000-0000-0000-000000000000");
    let out = run(&home, &home, &["show", "head0001", "--turns", "..2"]);
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    let s = stdout(&out);
    assert!(s.contains("Q1"));
    assert!(s.contains("A1"));
    assert!(!s.contains("Q2"));
    assert!(!s.contains("A3"));
    assert!(s.contains("turns 1–2 of 6"));
}

#[test]
fn show_turns_tail_keeps_only_last_n() {
    let home = fresh_home("show-tail");
    write_multiturn(&home, "tail0001-0000-0000-0000-000000000000");
    let out = run(&home, &home, &["show", "tail0001", "--turns", "-2.."]);
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    let s = stdout(&out);
    assert!(!s.contains("Q1"));
    assert!(!s.contains("A2"));
    assert!(s.contains("Q3"));
    assert!(s.contains("A3"));
    assert!(s.contains("turns 5–6 of 6"));
}

#[test]
fn show_turns_single_and_negative() {
    let home = fresh_home("show-single");
    write_multiturn(&home, "sngl0001-0000-0000-0000-000000000000");

    let out = run(&home, &home, &["show", "sngl0001", "--turns", "3"]);
    assert!(out.status.success());
    let s = stdout(&out);
    assert!(s.contains("Q2"));
    assert!(!s.contains("Q1") && !s.contains("A2"));

    let out = run(&home, &home, &["show", "sngl0001", "--turns", "-1"]);
    assert!(out.status.success());
    let s = stdout(&out);
    assert!(s.contains("A3"));
    assert!(!s.contains("Q3"));
}

#[test]
fn show_turns_range_inclusive() {
    let home = fresh_home("show-range");
    write_multiturn(&home, "rnge0001-0000-0000-0000-000000000000");
    let out = run(&home, &home, &["show", "rnge0001", "--turns", "2..4"]);
    assert!(out.status.success());
    let s = stdout(&out);
    assert!(s.contains("A1"));
    assert!(s.contains("Q2"));
    assert!(s.contains("A2"));
    assert!(!s.contains("Q1"));
    assert!(!s.contains("Q3"));
    assert!(s.contains("turns 2–4 of 6"));
}

#[test]
fn show_role_user_keeps_only_user_prompts() {
    let home = fresh_home("show-role-user");
    let p = home.join(".claude/projects/-role");
    write_jsonl(
        &p,
        "role0001-0000-0000-0000-000000000000",
        &[
            r#"{"type":"user","message":{"content":"PROMPT-ONE"}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"REPLY-ONE"},{"type":"tool_use","name":"Bash","input":{"command":"ls"}}]}}"#,
            r#"{"type":"user","message":{"content":[{"type":"tool_result","content":[{"type":"text","text":"TOOL-OUT"}]}]}}"#,
            r#"{"type":"user","message":{"content":"PROMPT-TWO"}}"#,
            r#"{"type":"assistant","message":{"content":"REPLY-TWO"}}"#,
        ],
    );
    let out = run(&home, &home, &["show", "role0001", "--role", "user"]);
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    let s = stdout(&out);
    assert!(s.contains("PROMPT-ONE"), "stdout: {s}");
    assert!(s.contains("PROMPT-TWO"));
    assert!(!s.contains("REPLY-ONE"), "should hide assistant");
    assert!(!s.contains("REPLY-TWO"));
    assert!(!s.contains("TOOL-OUT"), "should hide tool_result events");
    assert!(!s.contains("#2 tool"));
}

#[test]
fn show_role_assistant_keeps_only_assistant_turns() {
    let home = fresh_home("show-role-asst");
    let p = home.join(".claude/projects/-role");
    write_jsonl(
        &p,
        "rolea001-0000-0000-0000-000000000000",
        &[
            r#"{"type":"user","message":{"content":"PROMPT"}}"#,
            r#"{"type":"assistant","message":{"content":"REPLY"}}"#,
        ],
    );
    let out = run(&home, &home, &["show", "rolea001", "--role", "assistant"]);
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    let s = stdout(&out);
    assert!(s.contains("REPLY"));
    assert!(!s.contains("PROMPT"));
}

#[test]
fn show_turns_single_out_of_range_does_not_silently_clamp() {
    // Regression: `cch show <id> --turns 999` on a 6-turn session silently
    // rendered turn 6 (clamped). That's inconsistent with `--turns 0`, which
    // errors, and it masks user typos. Out-of-range must either error or
    // render an empty selection — it must NOT show a different turn as if
    // the user had asked for it.
    let home = fresh_home("show-oor");
    write_multiturn(&home, "oorr0001-0000-0000-0000-000000000000");
    let out = run(&home, &home, &["show", "oorr0001", "--turns", "999"]);
    let s = stdout(&out);
    // Turn 6's content ("A3") must not appear — user asked for 999, not 6.
    assert!(
        !s.contains("A3"),
        "--turns 999 silently rendered the last turn instead of erroring:\n{s}"
    );
    // Acceptable outcomes: non-zero exit (error) OR empty selection footer.
    let acceptable = !out.status.success() || s.contains("0 turns");
    assert!(
        acceptable,
        "--turns 999 must error or show an empty selection. exit={:?} stdout:\n{s}\nstderr:\n{}",
        out.status.code(),
        stderr(&out)
    );
}

#[test]
fn show_turns_invalid_spec_errors() {
    let home = fresh_home("show-bad");
    write_multiturn(&home, "badd0001-0000-0000-0000-000000000000");
    let out = run(&home, &home, &["show", "badd0001", "--turns", "abc"]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("--turns"));
}

// --- grep (extended coverage; pre-refactor regression net) -------------------

/// Pull the role-tagged hit lines out of a grep output (skips the header /
/// preview / footer lines so we can count actual matches deterministically).
fn grep_hit_lines(s: &str, role_tag: &str) -> Vec<String> {
    s.lines()
        .filter(|l| l.trim_start().starts_with(role_tag))
        .map(|l| l.to_string())
        .collect()
}

#[test]
fn grep_regex_flag_matches_alternation() {
    let home = fresh_home("grep-rx-alt");
    let p = home.join(".claude/projects/-proj");
    write_jsonl(
        &p,
        "rxalt001-0000-0000-0000-000000000000",
        &[
            r#"{"type":"user","message":{"content":"alpha line"}}"#,
            r#"{"type":"assistant","message":{"content":"bravo line"}}"#,
            r#"{"type":"user","message":{"content":"charlie line"}}"#,
        ],
    );
    let out = run(&home, &home, &["grep", "-E", "alpha|charlie"]);
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    let s = stdout(&out);
    assert!(s.contains("alpha"));
    assert!(s.contains("charlie"));
    assert!(!s
        .lines()
        .any(|l| l.contains("bravo line") && !l.contains("alpha")));
}

#[test]
fn grep_invalid_regex_errors() {
    let home = fresh_home("grep-rx-bad");
    let p = home.join(".claude/projects/-proj");
    write_jsonl(
        &p,
        "rxbad001-0000-0000-0000-000000000000",
        &[r#"{"type":"user","message":{"content":"hello"}}"#],
    );
    let out = run(&home, &home, &["grep", "-E", "("]);
    assert!(!out.status.success());
    let err = stderr(&out).to_lowercase();
    assert!(
        err.contains("regex") || err.contains("invalid") || err.contains("parse"),
        "expected regex error mention, got: {err}"
    );
}

#[test]
fn grep_no_tools_excludes_tool_result_hits() {
    let home = fresh_home("grep-no-tools");
    let p = home.join(".claude/projects/-proj");
    write_jsonl(
        &p,
        "ntool001-0000-0000-0000-000000000000",
        &[
            r#"{"type":"user","message":{"content":"prompt"}}"#,
            r#"{"type":"user","message":{"content":[{"type":"tool_result","content":"NTNEEDLE in bash dump"}]}}"#,
            r#"{"type":"assistant","message":{"content":"NTNEEDLE in reply"}}"#,
        ],
    );
    // Without --no-tools: both the tool_result and the assistant hit show up.
    let out = run(&home, &home, &["grep", "NTNEEDLE"]);
    assert!(out.status.success());
    let s = stdout(&out);
    let tool_hits = grep_hit_lines(&s, "tool");
    let asst_hits = grep_hit_lines(&s, "assistant");
    assert!(!tool_hits.is_empty(), "expected tool hit without -T: {s}");
    assert_eq!(asst_hits.len(), 1, "expected one assistant hit: {s}");

    // With --no-tools: only the assistant text remains.
    let out = run(&home, &home, &["grep", "NTNEEDLE", "-T"]);
    assert!(out.status.success());
    let s = stdout(&out);
    assert!(
        grep_hit_lines(&s, "tool").is_empty(),
        "tool leaked with -T: {s}"
    );
    assert_eq!(grep_hit_lines(&s, "assistant").len(), 1, "{s}");
}

#[test]
fn grep_turns_gates_matches_but_lets_context_span_outside() {
    let home = fresh_home("grep-turns-ctx");
    let p = home.join(".claude/projects/-proj");
    write_jsonl(
        &p,
        "trnctx01-0000-0000-0000-000000000000",
        &[
            r#"{"type":"user","message":{"content":"TRNK one"}}"#,
            r#"{"type":"assistant","message":{"content":"TRNK two"}}"#,
            r#"{"type":"user","message":{"content":"TRNK three"}}"#,
            r#"{"type":"assistant","message":{"content":"TRNK four"}}"#,
        ],
    );

    // --turns -1 alone: match only on turn 4.
    let out = run(&home, &home, &["grep", "TRNK", "--turns", "-1"]);
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    let s = stdout(&out);
    // Only role-tagged hit lines count as matches (the preview echoes the
    // session's first prompt regardless of --turns). Exactly one hit on #4.
    let hit_rows: Vec<_> = s
        .lines()
        .filter(|l| {
            let t = l.trim_start();
            (t.starts_with("user") || t.starts_with("assistant")) && t.contains("TRNK")
        })
        .collect();
    assert_eq!(hit_rows.len(), 1, "expected one match row: {s}");
    assert!(hit_rows[0].contains("#4"), "match row not on turn 4: {s}");

    // With -B 2 the context spans turns 2 & 3 (outside the --turns window).
    let out = run(&home, &home, &["grep", "TRNK", "--turns", "-1", "-B", "2"]);
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    let s = stdout(&out);
    assert!(s.contains("TRNK two"), "context turn 2 missing: {s}");
    assert!(s.contains("TRNK three"), "context turn 3 missing: {s}");
}

#[test]
fn grep_context_c_renders_dim_dash_turn_for_other_role() {
    // -C 1 around an assistant match should surface the surrounding user turns
    // as context rows. Verifies turn numbers and role labels render in output.
    let home = fresh_home("grep-ctx-C");
    let p = home.join(".claude/projects/-proj");
    write_jsonl(
        &p,
        "ctxc0001-0000-0000-0000-000000000000",
        &[
            r#"{"type":"user","message":{"content":"the question"}}"#,
            r#"{"type":"assistant","message":{"content":"answer with CTXNDL inside"}}"#,
            r#"{"type":"user","message":{"content":"the followup"}}"#,
        ],
    );
    let out = run(&home, &home, &["grep", "CTXNDL", "-C", "1"]);
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    let s = stdout(&out);
    assert!(s.contains("the question"), "before-ctx missing: {s}");
    assert!(s.contains("CTXNDL"));
    assert!(s.contains("the followup"), "after-ctx missing: {s}");
    // Match is on turn #2.
    assert!(s.contains("#2"), "turn marker missing: {s}");
    // Context turns also carry their numbers.
    assert!(s.contains("#1"));
    assert!(s.contains("#3"));
}

#[test]
fn grep_json_emits_one_object_per_match_with_keys() {
    let home = fresh_home("grep-json");
    let p = home.join(".claude/projects/-proj");
    write_jsonl(
        &p,
        "jsonout1-0000-0000-0000-000000000000",
        &[
            r#"{"type":"user","timestamp":"2026-04-23T10:00:00.000Z","message":{"content":"JSNDL one"}}"#,
            r#"{"type":"assistant","timestamp":"2026-04-23T10:00:01.000Z","message":{"content":"JSNDL two"}}"#,
        ],
    );
    // Add -B 1 to confirm context rows are dropped from JSON output.
    let out = run(&home, &home, &["grep", "JSNDL", "--json", "-B", "1"]);
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    let s = stdout(&out);
    let lines: Vec<&str> = s.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(lines.len(), 2, "expected 2 match objects, got: {s}");
    for l in &lines {
        let v: serde_json::Value =
            serde_json::from_str(l).unwrap_or_else(|e| panic!("bad json {l:?}: {e}"));
        for key in [
            "session",
            "project",
            "timestamp",
            "turn",
            "role",
            "tool",
            "match",
        ] {
            assert!(v.get(key).is_some(), "json missing {key}: {l}");
        }
        // No ANSI escapes in match.
        let m = v.get("match").and_then(|x| x.as_str()).unwrap_or("");
        assert!(!m.contains('\x1b'), "ANSI in json match: {m}");
        assert!(m.contains("JSNDL"));
    }
}

#[test]
fn grep_stats_summarizes_matches_sessions_and_time_range() {
    let home = fresh_home("grep-stats");
    let p = home.join(".claude/projects/-proj");
    write_jsonl(
        &p,
        "stats001-0000-0000-0000-000000000000",
        &[
            r#"{"type":"user","timestamp":"2026-03-29T08:00:00.000Z","message":{"content":"STNDL first"}}"#,
            r#"{"type":"assistant","timestamp":"2026-03-29T08:00:01.000Z","message":{"content":"STNDL second"}}"#,
        ],
    );
    write_jsonl(
        &p,
        "stats002-0000-0000-0000-000000000000",
        &[
            r#"{"type":"user","timestamp":"2026-04-24T09:00:00.000Z","message":{"content":"STNDL third"}}"#,
        ],
    );
    write_jsonl(
        &p,
        "stats003-0000-0000-0000-000000000000",
        &[r#"{"type":"user","message":{"content":"unrelated"}}"#],
    );

    let out = run(&home, &home, &["grep", "STNDL", "--stats"]);
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    let s = stdout(&out);
    let lines: Vec<&str> = s.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(lines.len(), 1, "expected single summary line, got: {s:?}");
    let line = lines[0];
    assert!(line.contains("3 matches"), "missing count: {line}");
    assert!(line.contains("2 sessions"), "missing sessions: {line}");
    assert!(
        line.contains("2026-03-29 08:00:00"),
        "missing first ts: {line}"
    );
    assert!(
        line.contains("2026-04-24 09:00:00"),
        "missing last ts: {line}"
    );
    assert!(line.contains("→"), "missing range arrow: {line}");

    // No-match path still exits 1.
    let out = run(&home, &home, &["grep", "no-such-needle-xyz", "--stats"]);
    assert_eq!(out.status.code(), Some(1));

    // Conflicts with --json and -l.
    let out = run(&home, &home, &["grep", "STNDL", "--stats", "--json"]);
    assert!(!out.status.success());
    let out = run(&home, &home, &["grep", "STNDL", "--stats", "-l"]);
    assert!(!out.status.success());
}

#[test]
fn grep_files_with_matches_prints_only_session_id() {
    let home = fresh_home("grep-l");
    let p = home.join(".claude/projects/-proj");
    write_jsonl(
        &p,
        "lhit0001-0000-0000-0000-000000000000",
        &[r#"{"type":"user","message":{"content":"LHIT here"}}"#],
    );
    write_jsonl(
        &p,
        "lmiss001-0000-0000-0000-000000000000",
        &[r#"{"type":"user","message":{"content":"unrelated"}}"#],
    );

    let out = run(&home, &home, &["grep", "LHIT", "-l"]);
    assert!(out.status.success());
    let s = stdout(&out);
    let lines: Vec<&str> = s.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(lines.len(), 1, "expected exactly one id line: {s:?}");
    assert_eq!(lines[0], "lhit0001-0000-0000-0000-000000000000");

    // No-match path.
    let out = run(&home, &home, &["grep", "no-such-needle-xyz", "-l"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(stdout(&out).is_empty());
}

#[test]
fn grep_project_restricts_to_named_project() {
    let home = fresh_home("grep-proj");
    let p_other = home.join(".claude/projects/-other-proj");
    let target_cwd = home.join("targetproj");
    std::fs::create_dir_all(&target_cwd).unwrap();
    let target_cwd = target_cwd.canonicalize().unwrap();
    let p_target = projects_dir_for(&home, &target_cwd);

    write_jsonl(
        &p_target,
        "ptgt0001-0000-0000-0000-000000000000",
        &[r#"{"type":"assistant","message":{"content":"PRJNDL match"}}"#],
    );
    write_jsonl(
        &p_other,
        "pother01-0000-0000-0000-000000000000",
        &[r#"{"type":"assistant","message":{"content":"PRJNDL match"}}"#],
    );

    // Use the path form of --project so it's deterministic across machines.
    let out = run(
        &home,
        &home,
        &["grep", "PRJNDL", "--project", target_cwd.to_str().unwrap()],
    );
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    let s = stdout(&out);
    assert!(s.contains("ptgt0001"));
    assert!(!s.contains("pother01"), "other project leaked: {s}");
}

#[test]
fn grep_max_matches_per_session_caps_at_five() {
    let home = fresh_home("grep-cap");
    let p = home.join(".claude/projects/-proj");
    // 8 events each containing the needle; cap is 5.
    let lines: Vec<String> = (0..8)
        .map(|i| format!(r#"{{"type":"assistant","message":{{"content":"CAPNDL {i}"}}}}"#))
        .collect();
    let refs: Vec<&str> = lines.iter().map(|s| s.as_str()).collect();
    write_jsonl(&p, "capx0001-0000-0000-0000-000000000000", &refs);

    let out = run(&home, &home, &["grep", "CAPNDL"]);
    assert!(out.status.success());
    let s = stdout(&out);
    let asst = grep_hit_lines(&s, "assistant");
    assert_eq!(asst.len(), 5, "cap should be 5, got {}: {s}", asst.len());
}

#[test]
fn grep_snippet_centers_long_line_around_match() {
    let home = fresh_home("grep-snip");
    let p = home.join(".claude/projects/-proj");
    // Long body — 600 chars of 'a', then SNPNDL, then 600 chars of 'b'.
    let long = format!("{}SNPNDL{}", "a".repeat(600), "b".repeat(600));
    let line = format!(
        r#"{{"type":"assistant","message":{{"content":{}}}}}"#,
        serde_json::Value::String(long)
    );
    write_jsonl(&p, "snip0001-0000-0000-0000-000000000000", &[&line]);

    let out = run(&home, &home, &["grep", "SNPNDL"]);
    assert!(out.status.success());
    let s = stdout(&out);
    let hit = s
        .lines()
        .find(|l| l.contains("SNPNDL"))
        .expect("hit line missing");
    assert!(hit.contains('…'), "expected truncation ellipsis: {hit:?}");
    // Snippet width is 140; full line is 1206 chars. The visible hit line
    // should be much shorter than the full body.
    assert!(
        hit.chars().count() < 300,
        "snippet too wide: {} chars",
        hit.chars().count()
    );
}

#[test]
fn grep_role_user_does_not_match_inside_tool_result() {
    // Bug pinned by commit 8d91b11: --role user must not match inside
    // tool_result payloads, even though the wrapping JSON event has type=user.
    let home = fresh_home("grep-roleuser-tr");
    let p = home.join(".claude/projects/-proj");
    write_jsonl(
        &p,
        "rutr0001-0000-0000-0000-000000000000",
        &[
            r#"{"type":"user","message":{"content":[{"type":"tool_result","content":"RUNDL in bash output"}]}}"#,
            r#"{"type":"user","message":{"content":"RUNDL typed by human"}}"#,
        ],
    );
    let out = run(&home, &home, &["grep", "RUNDL", "--role", "user"]);
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    let s = stdout(&out);
    // No `tool`-tagged hit must appear.
    assert!(
        grep_hit_lines(&s, "tool").is_empty(),
        "tool hit leaked: {s}"
    );
    // Exactly one `user`-tagged hit, and it's the human-typed one.
    let users = grep_hit_lines(&s, "user");
    assert_eq!(users.len(), 1, "want exactly one user hit: {s}");
    assert!(users[0].contains("typed by human"), "wrong user hit: {s}");
}

#[test]
fn grep_orders_sessions_most_recent_first() {
    let home = fresh_home("grep-order");
    let p = home.join(".claude/projects/-proj");
    let older = write_jsonl(
        &p,
        "ordold01-0000-0000-0000-000000000000",
        &[r#"{"type":"assistant","message":{"content":"ORDNDL old session"}}"#],
    );
    let newer = write_jsonl(
        &p,
        "ordnew01-0000-0000-0000-000000000000",
        &[r#"{"type":"assistant","message":{"content":"ORDNDL new session"}}"#],
    );
    set_mtime_secs(&older, 1_776_816_000); // 2026-04-22
    set_mtime_secs(&newer, 1_776_945_600); // 2026-04-23

    let out = run(&home, &home, &["grep", "ORDNDL"]);
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    let s = stdout(&out);
    let pos_new = s.find("ordnew01").expect("newer missing");
    let pos_old = s.find("ordold01").expect("older missing");
    assert!(
        pos_new < pos_old,
        "expected newer session first; got new@{pos_new} old@{pos_old}\n{s}"
    );
}

#[test]
fn grep_since_until_aliases_match_after_before() {
    let home = fresh_home("grep-aliases");
    let p = home.join(".claude/projects/-proj");
    write_jsonl(
        &p,
        "aliz0001-0000-0000-0000-000000000000",
        &[
            r#"{"type":"user","timestamp":"2026-04-22T12:00:00.000Z","message":{"content":"ALIZ early"}}"#,
            r#"{"type":"user","timestamp":"2026-04-23T16:30:00.000Z","message":{"content":"ALIZ middle"}}"#,
            r#"{"type":"user","timestamp":"2026-04-23T18:00:00.000Z","message":{"content":"ALIZ late"}}"#,
        ],
    );

    let out = run(
        &home,
        &home,
        &[
            "grep",
            "ALIZ",
            "--since",
            "2026-04-23",
            "--until",
            "2026-04-23T17:00",
        ],
    );
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    let s = stdout(&out);
    let hits: Vec<_> = s
        .lines()
        .filter(|l| l.trim_start().starts_with("user") && l.contains("ALIZ"))
        .collect();
    assert_eq!(hits.len(), 1, "want one hit, got: {s}");
    assert!(hits[0].contains("middle"), "wrong hit: {s}");
}

#[test]
fn grep_match_exits_zero() {
    let home = fresh_home("grep-exit-0");
    let p = home.join(".claude/projects/-proj");
    write_jsonl(
        &p,
        "ex000001-0000-0000-0000-000000000000",
        &[r#"{"type":"assistant","message":{"content":"EXITNDL hit"}}"#],
    );
    let out = run(&home, &home, &["grep", "EXITNDL"]);
    assert_eq!(out.status.code(), Some(0));
}

#[test]
fn grep_here_from_non_project_cwd_exits_1_no_crash() {
    // CWD has no encoded project dir under ~/.claude/projects/. `--here`
    // should treat that as "no matches" and exit 1, not crash.
    let home = fresh_home("grep-here-empty");
    let cwd = home.join("nowhere");
    std::fs::create_dir_all(&cwd).unwrap();
    let cwd = cwd.canonicalize().unwrap();
    // Make ~/.claude/projects exist but with an unrelated project, so the
    // walk has something to skip past.
    let other = home.join(".claude/projects/-unrelated");
    write_jsonl(
        &other,
        "uxxxxxxx-0000-0000-0000-000000000000",
        &[r#"{"type":"assistant","message":{"content":"HEREMISS yes"}}"#],
    );

    let out = run(&home, &cwd, &["grep", "HEREMISS", "--here"]);
    assert_eq!(out.status.code(), Some(1), "stderr: {}", stderr(&out));
    assert!(stdout(&out).is_empty());
}
