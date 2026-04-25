# cch

Fast CLI to navigate [Claude Code](https://claude.com/claude-code) JSONL session transcripts (`~/.claude/projects/`). Built for humans and for agents.

> Disclaimer: this tool was entirely vibe-coded.

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

## License

Dual-licensed under MIT or Apache-2.0, at your option.
