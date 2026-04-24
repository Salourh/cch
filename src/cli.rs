use clap::{Parser, Subcommand, ValueEnum};

use crate::commands;
use crate::transcript::Role;

const ABOUT: &str = "Navigate Claude Code sessions";

const LONG_ABOUT: &str = "\
Navigate Claude Code sessions (JSONL transcripts in ~/.claude/projects/).

Like `git` for your conversations: list, grep, open. Sessions are UUIDs,
resolvable by unique prefix (e.g. `cch show a1b2`).

Common flows:
  cch session                        recent sessions in the current project
  cch session -3                     3 most recent, full prompts
  cch session --since 2026-04-01     active since that date
  cch grep \"auth\"                    all projects (case-insensitive; -s for exact)
  cch grep \"auth\" --here             current project only
  cch grep \"auth\" --project cch      another worktree (basename or path)
  cch grep \"auth\" --after 2026-04-01 events on/after that day (also on `session`)
  cch grep \"panic\" --role user      your prompts only (no tool output)
  cch grep \"git log\" -B 2            1–2 turns of context — find the prompt behind a tool hit
  cch grep \"panic\" -l                session ids only — pipe to cch show
  cch show <prefix>                  open a session
  cch blame <sha>                    which session produced a commit
  cch commits <prefix>               commits a session produced (inverse of blame)

Color off automatically off-TTY or with NO_COLOR=1.";

const SESSION_AFTER: &str = "\
Examples:
  cch session                      # 10 most recent, one line each
  cch session -3                   # 3 most recent, full prompt
  cch session -n 25                # 25 entries
  cch session -3 --head 5          # clip each prompt to 5 lines
  cch session --after 2026-04-23   # since that day
  cch session --after 2026-04-23 --before 2026-04-24   # just that day
  cch session --all                # include sessions with no user prompt
  cch session --project cch         # another worktree (basename or path)
  cch session --touched src/cli.rs     # sessions that edited that file
  cch session --touched src/commands   # any file under that dir
  cch session --produced-commit HEAD   # sessions that produced that commit

`--after`/`--before` filter on mtime (last activity). Formats: YYYY-MM-DD or
YYYY-MM-DDTHH[:MM[:SS]] (UTC). 10-line cap still applies — raise with `-n N`.

`--touched PATH` keeps sessions whose Edit/Write/MultiEdit/NotebookEdit hit
PATH: exact, any file under a dir, or basename (`cli.rs` matches any `/…/cli.rs`).
Path → sessions, vs `cch blame <sha>` (commit → session); catches uncommitted
changes.

`--produced-commit SHA`: reverse of `cch blame <sha>`, same scoring. Locks to
the commit's repo (ignores `--project`/cwd). Pipe ids into `cch show`/`cch grep`.

`--head N` clips each expanded prompt to N lines; clipped ones get a
`… K more lines — cch show <id>` hint. Only meaningful with `-n`.

Tip: `cch session -N` = head-style shortcut for `--count N`.";

const GREP_AFTER: &str = "\
Examples:
  cch grep \"memchr\"                       # all projects, case-insensitive literal
  cch grep \"TODO\" --here -s               # current project, case-sensitive
  cch grep \"TODO\" --project cch            # another worktree
  cch grep \"X\" -T                         # skip tool output — biggest noise cut
  cch grep -E 'P2\\.?5'                     # regex: P25 or P2.5
  cch grep -E 'choreographer|budget'        # regex: alternation
  cch grep \"panic\" --role assistant       # assistant turns only
  cch grep \"X\" --sidechains               # include subagent events (noisy)
  cch grep \"X\" --after 2026-04-23         # on/after that day
  cch grep \"X\" --before 2026-04-23T17:00  # strictly before that instant
  cch grep \"panic\" -C 2                    # 2 turns of context each side
  cch grep \"panic\" -B 1 -A 3               # 1 before, 3 after
  cch grep \"git log\" -B 2                   # surface the prompt behind a tool hit
  cch grep \"X\" --since 2026-04-23          # --after alias (git-style)
  cch grep \"X\" --turns -1                   # last turn only
  cch grep \"X\" --turns -1 --role assistant  # last assistant turn
  cch grep \"X\" --turns ..10                 # first 10 turns
  cch grep \"panic\" --json | jq .              # JSONL for scripts
  cch grep \"panic\" -l                          # session ids only
  cch grep \"X\" --reverse                      # oldest first — when was X introduced
  cch grep \"X\" --stats                        # N matches · M sessions · first → last

`-T/--no-tools` drops matches inside tool_result payloads (bash stdout, file
reads, grep dumps) — biggest single noise cut. Reach for it when a common
symbol floods output with machine chatter. Combine with `--role user` (what
you typed) or `--role assistant` (Claude's prose).

Date/time bounds: YYYY-MM-DD or YYYY-MM-DDTHH[:MM[:SS]] (UTC, trailing `Z`
optional). `--after` inclusive, `--before` exclusive; events with no timestamp
are dropped when either is set.

`-A`/`-B`/`-C` are turn-level, not text lines: `-C 2` = two turns each side.
Context ignores `--role`/`--turns` (so you see the other side of the
conversation) but respects `--sidechains` and date bounds. Context rows
dimmed; matches stay bold.

When a match lands inside tool output (e.g. `git log` bash output), `-B 1`/
`-B 2` surfaces the prompt that triggered the call — where the \"why\" lives.
Prefer `-B` over `--role user`, which excludes tool hits entirely (same
semantics as `cch show --role user`).

`--turns SPEC` uses the same grammar as `cch show --turns` (N, A..B, ..B,
A.., -N, -N..). With `--role`: `--turns -1 --role assistant` = last assistant
turn.

Patterns are literal by default (fast; `.` is a literal dot). `-E/--regex`
switches to Rust regex (same as `grep -E`: alternation, groups, quantifiers,
classes). Combine with `-s` for case-sensitive regex.

Matches group by session, most-recent first (flip with `--reverse`/`--oldest`
to find the first occurrence). Each hit shows its turn (e.g. `#42`) — feed to
`cch show <id> --turns 42` to jump in. Exits 1 on no match (grep-compatible).

`--json` emits JSONL on stdout with fields: session, project, timestamp, turn,
role, tool, match. Drops context rows; forces color off. With jq:
  cch grep panic --json | jq -r '[.session,.turn,.match] | @tsv'

`-l/--files-with-matches` prints one session id per line — pipe into `cch show`,
`cch commits`, or xargs. Short-circuits per session (fastest). Conflicts with
`--json`.

`--stats` replaces the listing with `N matches · M sessions · first → last`.
Honors every other filter. Conflicts with `--json` and `-l`.";

const SHOW_AFTER: &str = "\
Examples:
  cch show a1b2c3                  # full transcript, numbered turns
  cch show a1b2 --turns ..5        # first 5 turns (head)
  cch show a1b2 --turns -5..       # last 5 turns (tail)
  cch show a1b2 --turns 12         # turn 12 only
  cch show a1b2 --turns -1         # last turn only
  cch show a1b2 --turns 10..20     # turns 10–20 inclusive
  cch show a1b2 --role user        # user prompts only (no tool_result)
  cch show a1b2 --system           # include system events
  cch show a1b2 --sidechains       # include subagent turns

Diff two sessions' user intent:
  diff <(cch show A --role user) <(cch show B --role user)

Turns are numbered post-filter (sidechains/system skipped unless flagged).
Footer shows the total so you can narrow subsequent calls.

`--role user` keeps your prompts (tool_result events excluded). `--role
assistant` keeps Claude's replies. With `--turns`: `--role user --turns -1`
= last prompt.

--turns SPEC (1-indexed; negatives from end):
  N        single turn             A..B   inclusive range
  ..B      first B (head)          A..    from A to end
  -N       Nth from the end        -N..   last N (tail)

If the prefix matches multiple sessions, candidates are listed with their
project.";

const COMMITS_AFTER: &str = "\
Examples:
  cch commits a1b2c3               # commits authored by this session
  cch commits a1b2c3 --all         # include weaker matches (subject / sha / time)

Inverse of `cch blame <sha>`: walks `git log` in the session's repo within
its activity window (±1 day), keeping commits the session authored (ran
`git commit` via Bash with the subject or SHA in argv).

Repo is discovered from the first absolute `file_path` in a tool_use; falls
back to cwd's repo when its encoded path matches. Must be runnable from that
repo.

Columns: short-SHA · commit time · evidence tags · subject. Cross-check with
`cch blame`, or inspect with `git show`. Exits 1 when nothing matches.

Aliased as `cch log` for back-compat; primary name is `commits` since the
arg is a session prefix, not a commit-ish.";

const BLAME_AFTER: &str = "\
Examples:
  cch blame 5e069c9                # which session produced this commit
  cch blame HEAD                   # most recent commit
  cch blame HEAD~3                 # any commit-ish
  cch blame main..HEAD             # every commit in a range
  cch blame v1.2..v1.3             # tag range

Run from inside the commit's repo — sessions are located by top-level path.
Scores four signals:

  authored     session ran `git commit` via Bash with subject or SHA in
               argv (hard authorship)
  subject      commit subject appears verbatim in the transcript (strong —
               Claude usually writes it)
  sha          short/full SHA referenced in the session
  time-window  commit time falls between the session's first and last events

Signals are additive (200 / 100 / 50 / 10); highest-scoring session wins, up
to 3 candidates shown. A session is the author with `authored`, or when
`subject` appears inside its time-window — otherwise the match is citation
(a later session mentioning the commit). With no authoring evidence, exits 1;
if the commit predates the oldest retained session, notes the retention
window and lists up to 3 cite-only sessions. Fall back to `cch grep` with a
keyword from the diff.

Range form (`A..B`, same grammar as `git log A..B`) walks oldest-first, one
row per commit: short-SHA, winning session (or `--------`), tags, subject.
Consecutive rows with the same session blank the id to group runs. Exits 1
only when zero commits matched.

Reverse lookup: `cch session --produced-commit <sha>` lists sessions that
produced a commit — pipe the id into another cch command.";

#[derive(Parser)]
#[command(
    name = "cch",
    version,
    about = ABOUT,
    long_about = LONG_ABOUT,
    arg_required_else_help = true,
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Copy, Clone, ValueEnum)]
pub enum RoleArg {
    User,
    Assistant,
    Any,
}

impl RoleArg {
    fn to_role(self) -> Option<Role> {
        match self {
            RoleArg::User => Some(Role::User),
            RoleArg::Assistant => Some(Role::Assistant),
            RoleArg::Any => None,
        }
    }
}

#[derive(Subcommand)]
pub enum Command {
    /// List recent sessions for the current project (most recent first).
    #[command(after_help = SESSION_AFTER)]
    Session {
        /// Expand N most recent with full hash + full first prompt. `-N` = shortcut.
        #[arg(short = 'n', long = "count", value_name = "N")]
        count: Option<usize>,
        /// Also show sessions with no user prompt.
        #[arg(long)]
        all: bool,
        /// Keep sessions active at or after this instant.
        #[arg(long, value_name = "WHEN", alias = "since")]
        after: Option<String>,
        /// Keep sessions active strictly before this instant.
        #[arg(long, value_name = "WHEN", alias = "until")]
        before: Option<String>,
        /// Another project (basename or path) instead of cwd.
        #[arg(long, value_name = "NAME")]
        project: Option<String>,
        /// Keep only sessions that edited this file (or any under this dir).
        #[arg(long, value_name = "PATH")]
        touched: Option<String>,
        /// Keep only sessions that produced this commit (reverse `cch blame`).
        #[arg(long, value_name = "SHA")]
        produced_commit: Option<String>,
        /// Clip each expanded prompt to N lines (needs `-n`).
        #[arg(long, value_name = "N")]
        head: Option<usize>,
    },

    /// Search transcripts; matches grouped by session, recent first (--reverse flips).
    #[command(after_help = GREP_AFTER)]
    Grep {
        /// Literal, case-insensitive by default (use `-s` for exact case, `-E` for regex).
        pattern: String,
        /// Current project only.
        #[arg(long, conflicts_with = "project")]
        here: bool,
        /// Another project (basename or path).
        #[arg(long, value_name = "NAME")]
        project: Option<String>,
        /// Case-sensitive.
        #[arg(short = 's', long = "case-sensitive")]
        case_sensitive: bool,
        /// PATTERN is a regex (ERE-style).
        #[arg(short = 'E', long = "regex")]
        regex: bool,
        /// Skip tool output (bash dumps, file reads) — biggest noise cut.
        #[arg(short = 'T', long = "no-tools")]
        no_tools: bool,
        /// Filter by role (`user` excludes tool_result — same as `cch show --role user`).
        #[arg(long, value_enum, default_value = "any")]
        role: RoleArg,
        /// Include subagent (sidechain) events.
        #[arg(long)]
        sidechains: bool,
        /// Events at or after this instant (YYYY-MM-DD or YYYY-MM-DDTHH[:MM[:SS]]).
        #[arg(long, value_name = "WHEN", alias = "since")]
        after: Option<String>,
        /// Events strictly before this instant.
        #[arg(long, value_name = "WHEN", alias = "until")]
        before: Option<String>,
        /// N turns of context after each match (like grep -A).
        #[arg(short = 'A', long = "after-context", value_name = "N")]
        after_context: Option<usize>,
        /// N turns of context before each match (like grep -B).
        #[arg(short = 'B', long = "before-context", value_name = "N")]
        before_context: Option<usize>,
        /// N turns of context each side (= -A N -B N).
        #[arg(short = 'C', long = "context", value_name = "N")]
        context: Option<usize>,
        /// Restrict matches to turns in SPEC (see `cch show --turns`).
        #[arg(
            short = 't',
            long = "turns",
            value_name = "SPEC",
            allow_hyphen_values = true
        )]
        turns: Option<String>,
        /// Emit one JSON object per match (JSONL) for jq / scripts.
        #[arg(long)]
        json: bool,
        /// Session ids only (like `grep -l`).
        #[arg(short = 'l', long = "files-with-matches", conflicts_with = "json")]
        files_with_matches: bool,
        /// Oldest first — find when X was introduced.
        #[arg(long, alias = "oldest")]
        reverse: bool,
        /// Summary line: `N matches · M sessions · first → last`.
        #[arg(
            long,
            conflicts_with_all = ["json", "files_with_matches"],
        )]
        stats: bool,
    },

    /// Commits a session produced (inverse of `cch blame`).
    #[command(after_help = COMMITS_AFTER, alias = "log")]
    Commits {
        /// Session id or unique prefix.
        prefix: String,
        /// Include weaker matches (subject / sha / time-window).
        #[arg(long)]
        all: bool,
    },

    /// Which session produced a commit (subject / SHA / time match).
    #[command(after_help = BLAME_AFTER)]
    Blame {
        /// Commit-ish: SHA, HEAD, HEAD~2, tag… (defaults to HEAD).
        #[arg(default_value = "HEAD")]
        sha: String,
    },

    /// Print the long help for every subcommand.
    #[command(name = "help-all")]
    HelpAll,

    /// Render a transcript (resolves by unique id prefix).
    #[command(after_help = SHOW_AFTER)]
    Show {
        /// Session id or unique prefix.
        prefix: String,
        /// Include subagent (sidechain) events.
        #[arg(long)]
        sidechains: bool,
        /// Include `system` events.
        #[arg(long)]
        system: bool,
        /// Keep only this role (user excludes tool_result).
        #[arg(long, value_enum, default_value = "any")]
        role: RoleArg,
        /// Turn selector: N, A..B, ..B, A.., -N, -N.. (1-indexed; negatives from end).
        #[arg(
            short = 't',
            long = "turns",
            value_name = "SPEC",
            allow_hyphen_values = true
        )]
        turns: Option<String>,
    },
}

pub fn dispatch(cli: Cli) -> anyhow::Result<()> {
    match cli.command {
        Command::Session {
            count,
            all,
            after,
            before,
            project,
            touched,
            produced_commit,
            head,
        } => {
            let after = after.map(|s| commands::grep::parse_bound(&s)).transpose()?;
            let before = before
                .map(|s| commands::grep::parse_bound(&s))
                .transpose()?;
            let project = project
                .as_deref()
                .map(crate::paths::resolve_project)
                .transpose()?;
            commands::session::run(commands::session::Opts {
                count,
                include_empty: all,
                after,
                before,
                project,
                touched,
                produced_commit,
                head,
            })
        }
        Command::Grep {
            pattern,
            here,
            project,
            case_sensitive,
            regex,
            role,
            sidechains,
            no_tools,
            after,
            before,
            after_context,
            before_context,
            context,
            turns,
            json,
            files_with_matches,
            reverse,
            stats,
        } => {
            let after = after.map(|s| commands::grep::parse_bound(&s)).transpose()?;
            let before = before
                .map(|s| commands::grep::parse_bound(&s))
                .transpose()?;
            let project = project
                .as_deref()
                .map(crate::paths::resolve_project)
                .transpose()?;
            let ctx_after = after_context.or(context).unwrap_or(0);
            let ctx_before = before_context.or(context).unwrap_or(0);
            let turns = turns
                .as_deref()
                .map(commands::show::TurnSpec::parse)
                .transpose()?;
            let code = commands::grep::run(commands::grep::Opts {
                pattern,
                here,
                project,
                case_sensitive,
                regex,
                role: role.to_role(),
                include_sidechains: sidechains,
                no_tools,
                after,
                before,
                context_before: ctx_before,
                context_after: ctx_after,
                turns,
                json,
                files_with_matches,
                reverse,
                stats,
            })?;
            if code != 0 {
                std::process::exit(code);
            }
            Ok(())
        }
        Command::Commits { prefix, all } => {
            let code = commands::commits::run(commands::commits::Opts { prefix, all })?;
            if code != 0 {
                std::process::exit(code);
            }
            Ok(())
        }
        Command::Blame { sha } => {
            let code = commands::blame::run(commands::blame::Opts { sha })?;
            if code != 0 {
                std::process::exit(code);
            }
            Ok(())
        }
        Command::HelpAll => commands::help_all::run(),
        Command::Show {
            prefix,
            sidechains,
            system,
            role,
            turns,
        } => commands::show::run(commands::show::Opts {
            prefix,
            include_sidechains: sidechains,
            include_system: system,
            role: role.to_role(),
            turns,
        }),
    }
}
