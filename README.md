# cch

Fast CLI to navigate [Claude Code](https://claude.com/claude-code) JSONL session transcripts (`~/.claude/projects/`). Built for humans and for agents.

> Disclaimer: this tool was entirely vibe-coded.

## Quick start

```bash
npm i -g cch-tool            # pre-built binary, no Rust needed
# or
cargo install cch            # build from source

cch help
cch help-all                 # full reference for every subcommand
```

## Use cases

**Humans**

```bash
#Check last 3 sessions
cch session -3

# "Check the first mention to Opus"
cch grep "Opus" --reverse
```

**Agents**

```bash
# "Which session produced this commit, and what was the user's exact prompt?"
cch blame <sha>
cch show <session-id> --role user

# "List every commit this session shipped, then check if it touched anything outside working folder."
cch commits <session-id>

# "Reconstruct how the Haiku-vs-Sonnet decision evolved — oldest first."
cch grep -E "Haiku|Sonnet" --role user --reverse -l
# then: cch show <id> --turns <range> on each hit
```

## secret Tip 

Add to your `CLAUDE.md`:

> Use the `cch` CLI whenever you need to navigate Claude Code JSONL session transcripts. Run `cch help-all` to discover every subcommand.

## How transcripts are resolved

- **Project**: cwd canonicalized, then every `/` and `.` → `-`. So `/home/a.b/c` → `~/.claude/projects/-home-a-b-c/`.
- **Session id**: any unique UUID prefix works (`cch show a1b2`).
- **Sidechains** (subagent calls) are skipped by default.

## License

Dual-licensed under MIT or Apache-2.0, at your option.
