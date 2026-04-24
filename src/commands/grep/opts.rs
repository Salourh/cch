//! Public options struct + internal hit representation.

use std::path::PathBuf;
use std::time::SystemTime;

use crate::commands::show::TurnSpec;
use crate::transcript::Role;

pub struct Opts {
    pub pattern: String,
    pub here: bool,
    /// Restrict to this already-resolved project dir. Conflicts with `here`.
    pub project: Option<PathBuf>,
    pub case_sensitive: bool,
    /// Interpret `pattern` as a regex (grep -E flavor) instead of a literal.
    pub regex: bool,
    pub role: Option<Role>,
    pub include_sidechains: bool,
    /// Skip matches inside tool_result parts (bash output, file reads, etc.) —
    /// big noise reducer when searching for symbols that appear in dumps.
    pub no_tools: bool,
    /// Inclusive lower bound, normalized to `YYYY-MM-DDTHH:MM:SS`.
    pub after: Option<String>,
    /// Exclusive upper bound, normalized to `YYYY-MM-DDTHH:MM:SS`.
    pub before: Option<String>,
    /// Events of context to show before each match (grep -B).
    pub context_before: usize,
    /// Events of context to show after each match (grep -A).
    pub context_after: usize,
    /// Restrict matches to turns matching this spec (same numbering as `cch show`).
    /// Gates matches only — `-A`/`-B` context still spans outside the window.
    pub turns: Option<TurnSpec>,
    /// Emit one JSON object per match on stdout (JSONL). Context hits are
    /// dropped; color is forced off so output is deterministic when piped.
    pub json: bool,
    /// Print only the session id of each file with at least one match, one
    /// per line — like `grep -l`. Short-circuits per session (stops at first
    /// hit, no snippet building), so it's the fastest listing mode.
    pub files_with_matches: bool,
    /// Oldest sessions first instead of most-recent-first — useful to find
    /// the first occurrence of a pattern ("when did I introduce X?").
    pub reverse: bool,
    /// Summary mode: print a one-line count/sessions/first↔last recap and
    /// skip the per-session listing. Honors all other filters.
    pub stats: bool,
}

pub(super) struct SessionHits {
    pub path: PathBuf,
    pub mtime: SystemTime,
    pub first_prompt: Option<String>,
    pub hits: Vec<Hit>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum HitKind {
    Match,
    Context,
}

pub(super) struct Hit {
    pub kind: HitKind,
    pub role: Role,
    pub is_tool: bool,
    pub timestamp: Option<String>,
    /// Turn number matching `cch show`'s default numbering — so the user can
    /// run `cch show <id> --turns N` to land on this hit. `None` when the
    /// event isn't part of the default view (e.g. an included sidechain).
    pub turn: Option<usize>,
    pub snippet: String,
}
