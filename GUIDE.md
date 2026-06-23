# loopctl — Getting Started Guide

Orchestrate persistent Claude Code agent sessions from your terminal. Each loop runs in its own terminal tab (or window depending on terminal plugin used) with automatic health checks, scheduling, and restart resilience.

## Core Concepts

| Concept | Description |
|---------|-------------|
| **Loop** | A persistent Claude Code session with a resume prompt, working directory, and schedule. Each loop gets its own terminal tab (or window depending on terminal plugin used). |
| **Loops Repo** | A directory with a `README.md` defining loops (headings + fenced code blocks) and a generated `workbench.toml` config. |
| **Daemon** | A background process that owns scheduling, health checks, and auto-recovery. Communicates via a Unix socket. |
| **Group** | An iTerm2 window. Loops in the same group share a window as separate tabs. On Linux/KDE, groups map to virtual desktops. |

---

## 1. Install

Build from source (requires Rust 1.75+):

```sh
# Install Rust if you don't have it
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"

# Clone and build
git clone <repo-url> ~/loopctl
cd ~/loopctl
cargo build --release

# Put on PATH
mkdir -p ~/.local/bin
ln -sf "$(pwd)/target/release/loopctl" ~/.local/bin/loopctl
export PATH="$HOME/.local/bin:$PATH"  # add to ~/.zshrc
```

Verify it works:

```sh
loopctl --version
# loopctl 0.1.0
```

> **Prerequisite:** iTerm2 must be installed at `/Applications/iTerm.app` (macOS), or Konsole if on GNU/Linux.

## 2. Create Your First Loops Repo

A loops repo is just a directory with a `README.md`. Each `### heading` followed by a fenced code block defines a loop and its resume prompt.

```sh
# Create the repo structure
mkdir -p ~/claude-loops/code-reviewer
mkdir -p ~/claude-loops/bug-investigator
```

Create `~/claude-loops/README.md`:

````markdown
# My Agent Loops

### code-reviewer — reviews open PRs

```
You are a code review assistant. Check for:
1. Security vulnerabilities
2. Performance issues
3. Missing error handling

Start by running: gh pr list --state open
Then review each PR systematically.
```

### bug-investigator — hunts down flaky tests

```
You are investigating flaky tests in this repo.

Your workflow:
1. Check recent CI failures: gh run list --status failure
2. Look for patterns in failures
3. Identify the root cause
4. Propose a fix

Start now.
```
````

> **How it works:** The heading text (e.g., "code-reviewer — reviews open PRs") becomes the loop name (slug before the "—") and the fenced code block is the resume prompt that gets pasted into Claude when the loop starts.

## 3. Load the Repo

This discovers loops from README.md, generates `workbench.toml`, starts the daemon, and opens terminal tabs:

```sh
loopctl load-repo ~/claude-loops
```

Expected output:

```
loaded /Users/you/claude-loops (generated workbench.toml — 2 loops):
  code-reviewer                    → desktop 7
  bug-investigator                 → desktop 8
user config now points at this repo.
daemon (re)started: pid 12345 (log: ~/.local/state/agent-loop-workbench/loopctl.log)
loopctl up fired — every loop should now have a pane
```

A terminal window should appear with two tabs — each running `claude` with its resume prompt auto-pasted.

> **First time on macOS:** macOS will ask you to allow iTerm2 to be controlled by osascript. Click "OK" — this is how loopctl drives iTerm2.

## 4. Check Status

```sh
loopctl status
```

Output:

```
terminal: iterm2
loops: 2
  code-reviewer        group=7 pane=iterm2:78123:A1B2C3... last_tick=- next_tick=-
  bug-investigator     group=8 pane=iterm2:78123:D4E5F6... last_tick=- next_tick=-
```

For JSON (pipe to `jq`, use in scripts):

```sh
loopctl status --json
```

## 5. Interact With Loops

### Focus a loop's tab

```sh
loopctl attach code-reviewer
# iTerm2 comes to foreground, switches to that tab
```

### Force a tick (re-paste the resume prompt)

```sh
loopctl tick code-reviewer
```

### Temporarily disable a loop

```sh
loopctl close bug-investigator
# Tab is closed, loop won't be scheduled or health-checked

loopctl reopen bug-investigator
# Tab comes back, resume prompt pasted fresh
```

### Bring everything down / back up

```sh
loopctl down   # close all panes
loopctl up     # bring them all back (idempotent)
```

## 6. The TUI Dashboard

```sh
loopctl tui
```

Opens an interactive terminal UI:

```
┌─ loopctl ───────────────────────────────────────────────┐
│                                                         │
│  ▸ code-reviewer      group=7  ● running   tick: -     │
│    bug-investigator  group=8  ● running   tick: -      │
│                                                         │
│  ↑↓ select  t tick  a attach  c close  r refresh  q quit│
└─────────────────────────────────────────────────────────┘
```

| Key | Action |
|-----|--------|
| `↑` / `↓` | Select loop |
| `t` | Force-tick selected loop |
| `a` | Attach (focus its iTerm2 tab) |
| `c` | Close (deactivate) the loop |
| `R` | Force a recovery pass |
| `r` | Refresh status |
| `d` | Delete loop (destructive) |
| `q` | Quit TUI |

## 7. Add a New Loop (Without Editing Files)

```sh
loopctl add-loop \
  --name daily-standup \
  --group 1 \
  --schedule "daily 09:00 America/Los_Angeles" \
  --message "Summarize what each loop accomplished yesterday.
Check git logs in each loop directory.
Write a brief standup report to ./standup.md"
```

This appends to both `README.md` and `workbench.toml`, reloads the daemon, and spawns the new tab immediately.

### From a file instead of inline:

```sh
loopctl add-loop \
  --name research-agent \
  --group 1 \
  --prompt-file ~/prompts/research.md
```

### With a tick script (runs a script on schedule instead of pasting):

```sh
loopctl add-loop \
  --name metrics-collector \
  --group 1 \
  --schedule "every 10m" \
  --dispatch tick_script \
  --tick-script scripts/collect.py \
  --message "placeholder prompt"
```

## 8. Scheduling

Each loop can have an automatic schedule. Set it in `workbench.toml` or via `--schedule` when adding:

| Schedule | Meaning |
|----------|---------|
| `none` | Manual only (use `loopctl tick`) |
| `every 10m` | Fire every 10 minutes |
| `every 60m` | Fire every hour |
| `daily 09:00 America/Los_Angeles` | Once per day at 9am PT |
| `weekly Mon 08:00 America/Los_Angeles` | Every Monday at 8am PT |

When a scheduled tick fires, the daemon re-pastes the resume prompt (dispatch = `resume_prompt`) or runs a tick script (dispatch = `tick_script`).

## 9. Configuration

The generated `workbench.toml` controls everything:

```toml
schema_version = 1

[workbench]
claude_loops_dir = "/Users/you/claude-loops"
terminal         = "iterm2"          # or "konsole" on Linux
central_group    = "1"               # group that `down` won't close
claude_binary    = "claude"          # path to claude CLI
claude_args      = ["--dangerously-skip-permissions"]  # default args
paste_submit_delay_ms = 700          # ms between paste and Enter

[[loops]]
name          = "code-reviewer"
dir           = "code-reviewer"      # workdir relative to claude_loops_dir
readme_anchor = "code-reviewer--reviews-open-prs"
group_key     = "1"                  # which iTerm2 window
schedule      = "every 10m"
dispatch      = "resume_prompt"      # or "tick_script" or "none"
# claude_args = []                   # uncomment to use stock claude (with permission prompts)
```

### Key settings:

| Setting | Effect |
|---------|--------|
| `claude_args` | Workbench-wide default. `["--dangerously-skip-permissions"]` for hands-off loops. Per-loop `claude_args = []` opts out. |
| `paste_submit_delay_ms` | How long to wait after pasting before sending Enter. Increase if prompts don't auto-submit. |
| `group_key` | Loops with the same group_key share an iTerm2 window (as separate tabs). |
| `active` | Set to `false` to disable without deleting (same as `loopctl close`). |

## 10. Health Checks & Auto-Recovery

The daemon checks every 60 seconds whether each loop's iTerm2 session is still alive.

- If a tab is closed (by you or a crash), the daemon detects it and auto-respawns the tab with the resume prompt.
- If respawning fails (e.g., workdir deleted), the loop enters `failed-recreate` state visible in the TUI.
- Force an immediate health pass: `loopctl recover`

> **Daemon restart:** After rebuilding loopctl, run `loopctl restart` to pick up the new binary. Add `--up` to also bring all panes back immediately.

## 11. Real-World Example: Multi-Loop Workflow

Here's a complete setup for a team running 3 parallel agents:

```sh
# Create the structure
mkdir -p ~/my-loops/{pr-reviewer,test-fixer,docs-updater}

cat > ~/my-loops/README.md << 'HEREDOC'
# Team Loops

### pr-reviewer — reviews PRs and posts comments

```
You review pull requests for the team.

Workflow:
1. Run: gh pr list --state open --json number,title,author
2. For each PR not yet reviewed by you:
   - gh pr diff <number>
   - Review for bugs, security issues, style
   - Post a review comment: gh pr review <number> --comment --body "..."
3. After reviewing all, sleep and wait for the next tick.
```

### test-fixer — fixes failing CI

```
You monitor CI and fix failing tests.

1. Check: gh run list --status failure --limit 5
2. For failures, look at logs: gh run view <id> --log-failed
3. Identify the broken test and fix it
4. Push the fix on a branch and open a PR
```

### docs-updater — keeps docs in sync with code

```
You keep documentation up to date.

1. Check git log for recent changes to src/
2. For each changed file, verify the corresponding docs
3. Update any stale documentation
4. Commit and push changes
```
HEREDOC

# Load it all
loopctl load-repo ~/my-loops

# Check everything is running
loopctl status

# Open the dashboard
loopctl tui
```

## 12. Update a Loop's Prompt

Change what a loop does without restarting everything:

```sh
# Update inline
loopctl update-prompt pr-reviewer \
  --message "New instructions: only review PRs tagged 'needs-review'"

# Update and immediately paste into the running session
loopctl update-prompt pr-reviewer \
  --message "New instructions here" \
  --repaste

# Update from a file
loopctl update-prompt pr-reviewer --prompt-file ~/new-prompt.md --repaste
```

## 13. Delete a Loop

Permanently remove a loop (closes pane, removes config, deletes directory):

```sh
loopctl delete-loop docs-updater
# About to DELETE loop `docs-updater`. This will:
#   1. close any live pane
#   2. remove the [[loops]] entry from workbench.toml
#   3. strip the `### docs-updater...` block from README.md
#   4. recursively delete the loop directory
#
# Type the loop name `docs-updater` to confirm: docs-updater
# deleted `docs-updater`
```

Skip confirmation in scripts:

```sh
loopctl delete-loop docs-updater --yes
```

## 14. Troubleshooting

### Daemon won't start

```sh
# Check the log
cat ~/.local/state/agent-loop-workbench/loopctl.log

# Validate your config
loopctl init

# Check for stale socket
ls /tmp/loopctl-$(id -u)/agent-loop-workbench.sock
# If present but daemon isn't running, delete it:
rm /tmp/loopctl-$(id -u)/agent-loop-workbench.sock
```

### Prompts not auto-submitting

Increase the paste delay in `workbench.toml`:

```toml
paste_submit_delay_ms = 1200  # default is 700
```

### iTerm2 permission error

Go to System Settings → Privacy & Security → Automation and ensure your terminal / loopctl has permission to control iTerm2.

### Duplicate tabs appearing

This happens if you run `loopctl up` before the daemon has recorded the session. The daemon's health check will clean up within 60 seconds. Or run:

```sh
loopctl recover
```

---

## Command Reference

| Command | Description |
|---------|-------------|
| `loopctl` | Start daemon if not running; show next steps |
| `loopctl init` | Validate workbench.toml (dry run) |
| `loopctl load-repo <path-or-url>` | Discover loops, generate config, start daemon |
| `loopctl set-repo <path>` | Point user config at a different repo |
| `loopctl up` | Bring all active loops up |
| `loopctl down` | Close all non-central panes |
| `loopctl status [--json]` | Show loop states |
| `loopctl tui` | Interactive dashboard |
| `loopctl attach <name>` | Focus a loop's tab |
| `loopctl tick <name>` | Force-fire a loop now |
| `loopctl add-loop ...` | Append a new loop |
| `loopctl update-prompt <name> ...` | Change a loop's resume prompt |
| `loopctl close <name>` | Deactivate a loop |
| `loopctl reopen <name>` | Reactivate a loop |
| `loopctl delete-loop <name>` | Permanently remove a loop |
| `loopctl recover` | Force health check + recovery pass |
| `loopctl reload` | Re-read workbench.toml |
| `loopctl restart [--up]` | Restart daemon (new binary) |
| `loopctl daemon-stop` | Graceful shutdown |
