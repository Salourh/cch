//! Data access + event model for Claude Code transcripts.
//!
//! A transcript is a `.jsonl` file where each line is a JSON event. This module
//! parses those events into a uniform [`Event`] struct so presentation commands
//! (`cch show`, `cch grep`) can share a single extraction path.

use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

use serde::Deserialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    User,
    Assistant,
    System,
    Other,
}

impl Role {
    fn parse(s: &str) -> Self {
        match s {
            "user" => Role::User,
            "assistant" => Role::Assistant,
            "system" => Role::System,
            _ => Role::Other,
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::System => "system",
            Role::Other => "other",
        }
    }
}

#[derive(Debug, Clone)]
pub enum Part {
    Text(String),
    ToolUse {
        name: String,
        summary: String,
        /// `file_path` from the tool input, when present. Populated for tools
        /// that edit/read files (Edit, Write, MultiEdit, NotebookEdit, Read…).
        file_path: Option<String>,
    },
    ToolResult(String),
}

/// Tools that modify files on disk. Used by `cch session --touched`.
pub const EDIT_TOOLS: &[&str] = &["Edit", "Write", "MultiEdit", "NotebookEdit"];

impl Part {
    pub fn as_search_text(&self) -> &str {
        match self {
            Part::Text(s) | Part::ToolResult(s) => s.as_str(),
            Part::ToolUse { .. } => "",
        }
    }
}

#[derive(Debug)]
pub struct Event {
    pub role: Role,
    pub is_sidechain: bool,
    pub timestamp: Option<String>,
    pub parts: Vec<Part>,
}

impl Event {
    /// True if every part in the event is a tool_result (typical for "user"-typed
    /// events that only carry tool output — we relabel those as "tool" on display).
    pub fn is_tool_only(&self) -> bool {
        !self.parts.is_empty() && self.parts.iter().all(|p| matches!(p, Part::ToolResult(_)))
    }

    /// True if this event is counted in `cch show`'s default numbering.
    /// Mirrors the filter in `commands::show`: no sidechains, no system, no
    /// empty events, no pure-wrapper user events (`<system-reminder>`-only).
    pub fn is_default_visible(&self) -> bool {
        if self.is_sidechain || self.role == Role::System || self.parts.is_empty() {
            return false;
        }
        !self.is_wrapper_user()
    }

    /// Iterate over file paths edited by this event (Edit/Write/MultiEdit/NotebookEdit).
    pub fn edited_paths(&self) -> impl Iterator<Item = &str> {
        self.parts.iter().filter_map(|p| match p {
            Part::ToolUse {
                name,
                file_path: Some(fp),
                ..
            } if EDIT_TOOLS.contains(&name.as_str()) => Some(fp.as_str()),
            _ => None,
        })
    }

    /// Pure-wrapper user event: only text parts, all of which are empty or start
    /// with `<` (e.g. `<system-reminder>…`, `<command-message>…`).
    fn is_wrapper_user(&self) -> bool {
        if self.role != Role::User {
            return false;
        }
        self.parts.iter().all(|p| match p {
            Part::Text(s) => {
                let t = s.trim();
                t.is_empty() || t.starts_with('<')
            }
            _ => false,
        })
    }
}

#[derive(Deserialize)]
struct RawEvent {
    #[serde(rename = "type", default)]
    kind: Option<String>,
    #[serde(default, rename = "isSidechain")]
    is_sidechain: bool,
    #[serde(default)]
    timestamp: Option<String>,
    #[serde(default)]
    message: Option<RawMessage>,
}

#[derive(Deserialize)]
struct RawMessage {
    #[serde(default)]
    content: Option<serde_json::Value>,
}

pub fn parse_event(line: &str) -> Option<Event> {
    let raw: RawEvent = serde_json::from_str(line).ok()?;
    let role = Role::parse(raw.kind.as_deref().unwrap_or(""));
    let mut parts = Vec::new();
    if let Some(msg) = raw.message {
        if let Some(content) = msg.content {
            collect_parts(&content, &mut parts);
        }
    }
    Some(Event {
        role,
        is_sidechain: raw.is_sidechain,
        timestamp: raw.timestamp,
        parts,
    })
}

fn collect_parts(v: &serde_json::Value, out: &mut Vec<Part>) {
    match v {
        serde_json::Value::String(s) => out.push(Part::Text(s.clone())),
        serde_json::Value::Array(arr) => {
            for p in arr {
                let Some(obj) = p.as_object() else { continue };
                let t = obj.get("type").and_then(|x| x.as_str()).unwrap_or("");
                match t {
                    "text" => {
                        if let Some(t) = obj.get("text").and_then(|x| x.as_str()) {
                            out.push(Part::Text(t.to_string()));
                        }
                    }
                    "tool_use" => {
                        let name = obj
                            .get("name")
                            .and_then(|n| n.as_str())
                            .unwrap_or("?")
                            .to_string();
                        let input = obj.get("input");
                        let summary = tool_input_summary(input);
                        let file_path = input
                            .and_then(|v| v.as_object())
                            .and_then(|o| o.get("file_path"))
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string());
                        out.push(Part::ToolUse {
                            name,
                            summary,
                            file_path,
                        });
                    }
                    "tool_result" => {
                        if let Some(c) = obj.get("content") {
                            let mut sub = Vec::new();
                            collect_parts(c, &mut sub);
                            let mut s = String::new();
                            for sp in sub {
                                match sp {
                                    Part::Text(t) | Part::ToolResult(t) => {
                                        if !s.is_empty() {
                                            s.push('\n');
                                        }
                                        s.push_str(&t);
                                    }
                                    Part::ToolUse { .. } => {}
                                }
                            }
                            out.push(Part::ToolResult(s));
                        }
                    }
                    _ => {}
                }
            }
        }
        _ => {}
    }
}

fn tool_input_summary(v: Option<&serde_json::Value>) -> String {
    let Some(obj) = v.and_then(|x| x.as_object()) else {
        return String::new();
    };
    for k in ["command", "file_path", "path", "pattern", "query", "url"] {
        if let Some(val) = obj.get(k).and_then(|x| x.as_str()) {
            let trimmed: String = val.chars().take(120).collect();
            let trimmed = trimmed.replace('\n', " ");
            return format!("{k}={trimmed}");
        }
    }
    String::new()
}

/// Iterate all parseable events in a transcript. Invalid/empty lines are skipped.
pub fn iter_events(path: &Path) -> anyhow::Result<impl Iterator<Item = Event>> {
    let file = File::open(path)?;
    let reader = BufReader::new(file);
    Ok(reader.lines().map_while(Result::ok).filter_map(|l| {
        let t = l.trim();
        if t.is_empty() {
            None
        } else {
            parse_event(t)
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_string_content() {
        let line = r#"{"type":"user","message":{"content":"hello"}}"#;
        let ev = parse_event(line).unwrap();
        assert_eq!(ev.role, Role::User);
        assert_eq!(ev.parts.len(), 1);
        assert!(matches!(&ev.parts[0], Part::Text(s) if s == "hello"));
    }

    #[test]
    fn parses_array_content_with_tool_use() {
        let line = r#"{"type":"assistant","message":{"content":[
            {"type":"text","text":"running it"},
            {"type":"tool_use","name":"Bash","input":{"command":"ls -la"}}
        ]}}"#;
        let ev = parse_event(line).unwrap();
        assert_eq!(ev.role, Role::Assistant);
        assert_eq!(ev.parts.len(), 2);
        assert!(matches!(&ev.parts[0], Part::Text(s) if s == "running it"));
        match &ev.parts[1] {
            Part::ToolUse { name, summary, .. } => {
                assert_eq!(name, "Bash");
                assert_eq!(summary, "command=ls -la");
            }
            _ => panic!("expected tool_use"),
        }
    }

    #[test]
    fn parses_tool_result_with_nested_content() {
        let line = r#"{"type":"user","message":{"content":[
            {"type":"tool_result","content":[{"type":"text","text":"ok"}]}
        ]}}"#;
        let ev = parse_event(line).unwrap();
        assert!(ev.is_tool_only());
        assert!(matches!(&ev.parts[0], Part::ToolResult(s) if s == "ok"));
    }

    #[test]
    fn skips_invalid_line() {
        assert!(parse_event("not json").is_none());
    }

    #[test]
    fn parses_role_variants() {
        assert_eq!(Role::parse("user"), Role::User);
        assert_eq!(Role::parse("assistant"), Role::Assistant);
        assert_eq!(Role::parse("system"), Role::System);
        assert_eq!(Role::parse("summary"), Role::Other);
        assert_eq!(Role::parse(""), Role::Other);
    }

    #[test]
    fn parses_sidechain_flag() {
        let line = r#"{"type":"user","isSidechain":true,"message":{"content":"x"}}"#;
        let ev = parse_event(line).unwrap();
        assert!(ev.is_sidechain);
    }

    #[test]
    fn defaults_sidechain_to_false() {
        let line = r#"{"type":"user","message":{"content":"x"}}"#;
        let ev = parse_event(line).unwrap();
        assert!(!ev.is_sidechain);
    }

    #[test]
    fn captures_timestamp_when_present() {
        let line =
            r#"{"type":"user","timestamp":"2026-04-23T10:00:00Z","message":{"content":"x"}}"#;
        let ev = parse_event(line).unwrap();
        assert_eq!(ev.timestamp.as_deref(), Some("2026-04-23T10:00:00Z"));
    }

    #[test]
    fn missing_message_yields_no_parts() {
        let line = r#"{"type":"system"}"#;
        let ev = parse_event(line).unwrap();
        assert_eq!(ev.role, Role::System);
        assert!(ev.parts.is_empty());
    }

    #[test]
    fn is_tool_only_requires_nonempty() {
        let ev = Event {
            role: Role::User,
            is_sidechain: false,
            timestamp: None,
            parts: vec![],
        };
        assert!(!ev.is_tool_only());
    }

    #[test]
    fn is_tool_only_mixed_returns_false() {
        let ev = Event {
            role: Role::User,
            is_sidechain: false,
            timestamp: None,
            parts: vec![Part::ToolResult("r".into()), Part::Text("t".into())],
        };
        assert!(!ev.is_tool_only());
    }

    #[test]
    fn tool_use_picks_first_known_key() {
        let line = r#"{"type":"assistant","message":{"content":[
            {"type":"tool_use","name":"Edit","input":{"file_path":"/a/b.rs","pattern":"x"}}
        ]}}"#;
        let ev = parse_event(line).unwrap();
        match &ev.parts[0] {
            Part::ToolUse { name, summary, .. } => {
                assert_eq!(name, "Edit");
                // "file_path" comes before "pattern" in the preferred-keys list
                assert_eq!(summary, "file_path=/a/b.rs");
            }
            _ => panic!("expected tool_use"),
        }
    }

    #[test]
    fn tool_use_summary_trims_newlines_and_length() {
        let long: String = "a".repeat(200);
        let body = format!(
            r#"{{"type":"assistant","message":{{"content":[
                {{"type":"tool_use","name":"Bash","input":{{"command":"{cmd}\nmore"}}}}
            ]}}}}"#,
            cmd = long
        );
        let ev = parse_event(&body).unwrap();
        match &ev.parts[0] {
            Part::ToolUse { summary, .. } => {
                assert!(!summary.contains('\n'));
                assert!(summary.starts_with("command="));
                // 120 char cap on value
                let val_len = summary.trim_start_matches("command=").chars().count();
                assert_eq!(val_len, 120);
            }
            _ => panic!("expected tool_use"),
        }
    }

    #[test]
    fn tool_use_unknown_input_key_is_empty_summary() {
        let line = r#"{"type":"assistant","message":{"content":[
            {"type":"tool_use","name":"Mystery","input":{"foo":"bar"}}
        ]}}"#;
        let ev = parse_event(line).unwrap();
        match &ev.parts[0] {
            Part::ToolUse { summary, .. } => assert!(summary.is_empty()),
            _ => panic!(),
        }
    }

    #[test]
    fn tool_result_with_string_content() {
        let line = r#"{"type":"user","message":{"content":[
            {"type":"tool_result","content":"direct string"}
        ]}}"#;
        let ev = parse_event(line).unwrap();
        assert!(matches!(&ev.parts[0], Part::ToolResult(s) if s == "direct string"));
    }

    #[test]
    fn unknown_part_types_are_ignored() {
        let line = r#"{"type":"assistant","message":{"content":[
            {"type":"image","source":{}},
            {"type":"text","text":"after"}
        ]}}"#;
        let ev = parse_event(line).unwrap();
        assert_eq!(ev.parts.len(), 1);
        assert!(matches!(&ev.parts[0], Part::Text(s) if s == "after"));
    }

    #[test]
    fn as_search_text_excludes_tool_use() {
        let p = Part::ToolUse {
            name: "X".into(),
            summary: "file_path=/a".into(),
            file_path: Some("/a".into()),
        };
        assert_eq!(p.as_search_text(), "");
        let p = Part::Text("hello".into());
        assert_eq!(p.as_search_text(), "hello");
        let p = Part::ToolResult("res".into());
        assert_eq!(p.as_search_text(), "res");
    }

    #[test]
    fn role_label_roundtrip() {
        assert_eq!(Role::User.label(), "user");
        assert_eq!(Role::Assistant.label(), "assistant");
        assert_eq!(Role::System.label(), "system");
        assert_eq!(Role::Other.label(), "other");
    }

    #[test]
    fn iter_events_skips_blank_and_invalid_lines() {
        use std::io::Write;
        let dir = std::env::temp_dir().join(format!("cch-iter-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.jsonl");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, r#"{{"type":"user","message":{{"content":"a"}}}}"#).unwrap();
        writeln!(f).unwrap();
        writeln!(f, "not json").unwrap();
        writeln!(f, r#"{{"type":"assistant","message":{{"content":"b"}}}}"#).unwrap();
        drop(f);
        let evs: Vec<_> = iter_events(&path).unwrap().collect();
        assert_eq!(evs.len(), 2);
        assert_eq!(evs[0].role, Role::User);
        assert_eq!(evs[1].role, Role::Assistant);
        std::fs::remove_dir_all(&dir).ok();
    }
}
