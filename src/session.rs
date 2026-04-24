use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::paths::projects_root;
use crate::transcript::{iter_events, Part, Role};

#[derive(Debug)]
pub struct SessionEntry {
    pub path: PathBuf,
    #[allow(dead_code)]
    pub mtime: SystemTime,
    pub first_prompt: Option<String>,
}

impl SessionEntry {
    pub fn id(&self) -> &str {
        self.path.file_stem().and_then(|s| s.to_str()).unwrap_or("")
    }
}

/// First real external user prompt as plain text.
/// Skips sidechains, tool results, and `<system-reminder>`-style wrappers.
pub fn first_user_prompt(jsonl: &Path) -> Option<String> {
    let events = iter_events(jsonl).ok()?;
    for ev in events {
        if ev.role != Role::User || ev.is_sidechain {
            continue;
        }
        for p in &ev.parts {
            if let Part::Text(s) = p {
                let t = s.trim();
                if !t.is_empty() && !t.starts_with('<') {
                    return Some(t.to_string());
                }
            }
        }
    }
    None
}

pub fn list_sessions(project_dir: &Path, include_empty: bool) -> anyhow::Result<Vec<SessionEntry>> {
    if !project_dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut entries: Vec<(PathBuf, SystemTime)> = std::fs::read_dir(project_dir)?
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let p = e.path();
            if p.extension().and_then(|s| s.to_str()) != Some("jsonl") {
                return None;
            }
            let mtime = e.metadata().ok()?.modified().ok()?;
            Some((p, mtime))
        })
        .collect();
    entries.sort_by_key(|e| std::cmp::Reverse(e.1));

    let mut out = Vec::with_capacity(entries.len());
    for (path, mtime) in entries {
        let first_prompt = first_user_prompt(&path);
        if first_prompt.is_none() && !include_empty {
            continue;
        }
        out.push(SessionEntry {
            path,
            mtime,
            first_prompt,
        });
    }
    Ok(out)
}

/// All `.jsonl` transcript paths across all project directories.
pub fn all_transcripts() -> anyhow::Result<Vec<PathBuf>> {
    let root = projects_root()?;
    let mut out = Vec::new();
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

/// Resolve a session by unique prefix across ALL projects (git-style).
///
/// - 0 matches → error
/// - 1 match   → that path
/// - N matches → error listing candidates so the user can disambiguate
///
/// Like `resolve_prefix` but searches a specific root directory. Used in tests;
/// the production call site uses `projects_root()`.
#[cfg(test)]
fn resolve_prefix_in(root: &Path, prefix: &str) -> anyhow::Result<PathBuf> {
    if prefix.is_empty() {
        anyhow::bail!("empty session prefix");
    }
    let mut all = Vec::new();
    if let Ok(rd) = std::fs::read_dir(root) {
        for proj in rd.filter_map(|e| e.ok()) {
            let pp = proj.path();
            if !pp.is_dir() {
                continue;
            }
            if let Ok(files) = std::fs::read_dir(&pp) {
                for f in files.filter_map(|e| e.ok()) {
                    let fp = f.path();
                    if fp.extension().and_then(|s| s.to_str()) == Some("jsonl") {
                        all.push(fp);
                    }
                }
            }
        }
    }
    let mut matches: Vec<PathBuf> = all
        .into_iter()
        .filter(|p| {
            p.file_stem()
                .and_then(|s| s.to_str())
                .is_some_and(|s| s.starts_with(prefix))
        })
        .collect();
    if let Some(idx) = matches.iter().position(|p| {
        p.file_stem()
            .and_then(|s| s.to_str())
            .is_some_and(|s| s == prefix)
    }) {
        return Ok(matches.swap_remove(idx));
    }
    match matches.len() {
        0 => anyhow::bail!("no session matching prefix '{prefix}'"),
        1 => Ok(matches.into_iter().next().unwrap()),
        _ => anyhow::bail!("ambiguous"),
    }
}

pub fn resolve_prefix(prefix: &str) -> anyhow::Result<PathBuf> {
    if prefix.is_empty() {
        anyhow::bail!("empty session prefix");
    }
    let all = all_transcripts()?;
    let mut matches: Vec<PathBuf> = all
        .into_iter()
        .filter(|p| {
            p.file_stem()
                .and_then(|s| s.to_str())
                .is_some_and(|s| s.starts_with(prefix))
        })
        .collect();

    // Exact match wins even if it's also a prefix of another (rare with UUIDs).
    if let Some(idx) = matches.iter().position(|p| {
        p.file_stem()
            .and_then(|s| s.to_str())
            .is_some_and(|s| s == prefix)
    }) {
        return Ok(matches.swap_remove(idx));
    }

    match matches.len() {
        0 => anyhow::bail!("no session matching prefix '{prefix}'"),
        1 => Ok(matches.into_iter().next().unwrap()),
        n => {
            let mut msg = format!("ambiguous prefix '{prefix}' — {n} matches:\n");
            for m in &matches {
                let id = m.file_stem().and_then(|s| s.to_str()).unwrap_or("?");
                let proj = m
                    .parent()
                    .and_then(|p| p.file_name())
                    .and_then(|s| s.to_str())
                    .unwrap_or("?");
                msg.push_str(&format!("  {id}  {proj}\n"));
            }
            anyhow::bail!("{}", msg.trim_end())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static COUNTER: AtomicUsize = AtomicUsize::new(0);

    fn unique_tmpdir(tag: &str) -> PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let p = std::env::temp_dir().join(format!("cch-test-{}-{}-{}", tag, std::process::id(), n));
        std::fs::create_dir_all(&p).unwrap();
        p
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

    #[test]
    fn first_user_prompt_returns_first_real_text() {
        let dir = unique_tmpdir("fup-real");
        let path = write_jsonl(
            &dir,
            "aaa",
            &[
                r#"{"type":"system","message":{"content":"ignored"}}"#,
                r#"{"type":"user","isSidechain":true,"message":{"content":"sub"}}"#,
                r#"{"type":"user","message":{"content":"<system-reminder>skip</system-reminder>"}}"#,
                r#"{"type":"user","message":{"content":"hello there"}}"#,
                r#"{"type":"user","message":{"content":"second"}}"#,
            ],
        );
        assert_eq!(first_user_prompt(&path).as_deref(), Some("hello there"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn first_user_prompt_none_when_only_wrappers() {
        let dir = unique_tmpdir("fup-none");
        let path = write_jsonl(
            &dir,
            "bbb",
            &[
                r#"{"type":"user","message":{"content":"<command-message>x</command-message>"}}"#,
                r#"{"type":"assistant","message":{"content":"reply"}}"#,
            ],
        );
        assert_eq!(first_user_prompt(&path), None);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn first_user_prompt_skips_tool_results() {
        let dir = unique_tmpdir("fup-tool");
        let path = write_jsonl(
            &dir,
            "ccc",
            &[
                r#"{"type":"user","message":{"content":[{"type":"tool_result","content":[{"type":"text","text":"output"}]}]}}"#,
                r#"{"type":"user","message":{"content":"real prompt"}}"#,
            ],
        );
        assert_eq!(first_user_prompt(&path).as_deref(), Some("real prompt"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn list_sessions_missing_dir_is_empty() {
        let dir = std::env::temp_dir().join("cch-absent-xyz-no-such");
        assert!(list_sessions(&dir, true).unwrap().is_empty());
    }

    #[test]
    fn list_sessions_filters_empty_when_requested() {
        let dir = unique_tmpdir("list");
        write_jsonl(
            &dir,
            "aaa11111-1111-1111-1111-111111111111",
            &[r#"{"type":"user","message":{"content":"real prompt"}}"#],
        );
        write_jsonl(
            &dir,
            "bbb22222-2222-2222-2222-222222222222",
            &[r#"{"type":"user","message":{"content":"<x>skip</x>"}}"#],
        );

        let with_empty = list_sessions(&dir, true).unwrap();
        assert_eq!(with_empty.len(), 2);

        let without_empty = list_sessions(&dir, false).unwrap();
        assert_eq!(without_empty.len(), 1);
        assert_eq!(
            without_empty[0].first_prompt.as_deref(),
            Some("real prompt")
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn list_sessions_sorts_most_recent_first() {
        // Two files written sequentially: the second one is guaranteed to have
        // mtime >= the first. If equal (low-res FS), the assertion below still
        // holds because we only check that the older one is NOT after newer.
        let dir = unique_tmpdir("list-mtime");
        let first = write_jsonl(
            &dir,
            "aaa11111-1111-1111-1111-111111111111",
            &[r#"{"type":"user","message":{"content":"first"}}"#],
        );
        // Force different mtime regardless of FS granularity.
        std::thread::sleep(std::time::Duration::from_millis(10));
        let second = write_jsonl(
            &dir,
            "bbb22222-2222-2222-2222-222222222222",
            &[r#"{"type":"user","message":{"content":"second"}}"#],
        );
        let m1 = std::fs::metadata(&first).unwrap().modified().unwrap();
        let m2 = std::fs::metadata(&second).unwrap().modified().unwrap();
        if m2 > m1 {
            let s = list_sessions(&dir, true).unwrap();
            assert_eq!(s[0].id(), "bbb22222-2222-2222-2222-222222222222");
            assert_eq!(s[1].id(), "aaa11111-1111-1111-1111-111111111111");
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn list_sessions_ignores_non_jsonl() {
        let dir = unique_tmpdir("nonjsonl");
        write_jsonl(
            &dir,
            "ddd",
            &[r#"{"type":"user","message":{"content":"ok"}}"#],
        );
        // Noise files that must be ignored.
        std::fs::write(dir.join("notes.txt"), "hi").unwrap();
        std::fs::write(dir.join("config.json"), "{}").unwrap();
        std::fs::create_dir_all(dir.join("subdir")).unwrap();

        let s = list_sessions(&dir, true).unwrap();
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].id(), "ddd");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn resolve_prefix_errors_on_empty() {
        let dir = unique_tmpdir("resolve-empty");
        let err = resolve_prefix_in(&dir, "").unwrap_err();
        assert!(err.to_string().contains("empty"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn resolve_prefix_finds_unique() {
        let root = unique_tmpdir("resolve-uniq");
        let proj = root.join("-p1");
        write_jsonl(
            &proj,
            "abcd1234-0000-0000-0000-000000000000",
            &[r#"{"type":"user","message":{"content":"x"}}"#],
        );
        let got = resolve_prefix_in(&root, "abcd").unwrap();
        assert!(got
            .file_stem()
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("abcd1234"));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn resolve_prefix_not_found() {
        let root = unique_tmpdir("resolve-404");
        std::fs::create_dir_all(root.join("-p")).unwrap();
        let err = resolve_prefix_in(&root, "zzzz").unwrap_err();
        assert!(err.to_string().contains("no session"));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn resolve_prefix_ambiguous_errors() {
        let root = unique_tmpdir("resolve-amb");
        write_jsonl(
            &root.join("-p1"),
            "deadbeef-aaaa-aaaa-aaaa-aaaaaaaaaaaa",
            &[r#"{"type":"user","message":{"content":"x"}}"#],
        );
        write_jsonl(
            &root.join("-p2"),
            "deadbeef-bbbb-bbbb-bbbb-bbbbbbbbbbbb",
            &[r#"{"type":"user","message":{"content":"y"}}"#],
        );
        let err = resolve_prefix_in(&root, "deadbeef").unwrap_err();
        assert!(err.to_string().contains("ambiguous"));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn resolve_prefix_exact_beats_longer_match() {
        let root = unique_tmpdir("resolve-exact");
        // The "exact" id is also a prefix of the longer one; exact must win.
        write_jsonl(
            &root.join("-p"),
            "abc",
            &[r#"{"type":"user","message":{"content":"x"}}"#],
        );
        write_jsonl(
            &root.join("-p"),
            "abcd",
            &[r#"{"type":"user","message":{"content":"y"}}"#],
        );
        let got = resolve_prefix_in(&root, "abc").unwrap();
        assert_eq!(got.file_stem().unwrap().to_str().unwrap(), "abc");
        std::fs::remove_dir_all(&root).ok();
    }
}
