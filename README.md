# cch

[![CI](https://github.com/Salourh/cch/actions/workflows/ci.yml/badge.svg)](https://github.com/Salourh/cch/actions/workflows/ci.yml)
[![npm](https://img.shields.io/npm/v/cch-tool.svg)](https://www.npmjs.com/package/cch-tool)
[![license](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)

Fast CLI to navigate [Claude Code](https://claude.com/claude-code) JSONL session transcripts (`~/.claude/projects/`). Git-inspired subcommands: `session`, `show`, `grep`, `blame`, `commits`. Built for humans and for agents.

~1ms for `cch session`, single static binary, no runtime.

![demo](demo.gif)

> Disclaimer: this tool was entirely vibe-coded — see [How this was built](#how-this-was-built).

## Quick start

Install the pre-built binary (no Rust needed):

```
npm i -g cch-tool
```

Or build from source:

```
cargo install --git https://github.com/Salourh/cch
```

Top-level help:

```
cch help
```

Full reference for every subcommand:

```
cch help-all
```

## Use cases

### Humans

Check the last 3 sessions:

```
cch session -3
```

Find the first mention of "Opus":

```
cch grep "Opus" --reverse
```

### Agents

Which session produced this commit, and what was the user's exact prompt:

```
cch blame <sha>
```

Show only user turns of a session:

```
cch show <session-id> --role user
```

List every commit a session shipped:

```
cch commits <session-id>
```

Reconstruct how the Haiku-vs-Sonnet decision evolved — oldest first:

```
cch grep -E "Haiku|Sonnet" --role user --reverse -l
```

Then open each hit turn-by-turn:

```
cch show <id> --turns <range>
```

## Secret tip

Add to your `CLAUDE.md`:

> Use the `cch` CLI whenever you need to navigate Claude Code JSONL session transcripts. Run `cch help-all` to discover every subcommand.

## How transcripts are resolved

- **Project**: cwd canonicalized, then every `/` and `.` → `-`. So `/home/a.b/c` → `~/.claude/projects/-home-a-b-c/`.
- **Session id**: any unique UUID prefix works (`cch show a1b2`).
- **Sidechains** (subagent calls) are skipped by default.

## How this was built

`cch` was vibe-coded end-to-end with Claude Code.

- `cargo clippy --all-targets -- -D warnings` on every change
- No `unwrap()` in production code, `anyhow::Result` everywhere at surface
- Sub-10ms startup as a hard budget, checked against real data
- CI runs fmt + clippy + tests on Linux and macOS; releases ship pre-built binaries for Linux (x86_64, aarch64), macOS (x86_64, aarch64), and Windows (x86_64)

Measured on a 192-project corpus (~2GB JSONL total):

| Command                 | Warm runtime |
| ----------------------- | ------------ |
| `cch session -n 10`     | ~1 ms        |
| `cch session --all`     | ~1 ms        |
| `cch grep "Opus" -l`    | ~850 ms      |

## License

Dual-licensed under MIT or Apache-2.0, at your option.
