# loopctl

A Rust CLI + TUI for orchestrating long-running coding-agent
"loops" — persistent agent sessions whose state lives in a markdown repo.
Each loop can run [Claude Code][claude-code], [Codex][codex], or
[opencode][opencode] (per-loop, selectable via the `agent` field).
Native to KDE Plasma: each loop runs as a Konsole tab on a chosen virtual
desktop, with a central daemon owning schedules, health checks, and
restart-resilient state.

[claude-code]: https://www.anthropic.com/claude-code
[codex]: https://github.com/openai/codex
[opencode]: https://opencode.ai

## Status

v1. The `konsole` terminal backend is the only one implemented; tmux and
macOS Terminal/iTerm2 stubs exist but return `NotImplemented`. X11 only
(Wayland deferred).

100+ tests across the lib + integration suites; `cargo clippy --
-D warnings` is clean.

## What it does

- **Discover loops** in a claude-loops repo (`### <heading>` blocks in
  README.md, mapped to top-level dirs).
- **Spawn one Konsole window per virtual desktop**, one tab per loop,
  with `claude` running in each.
- **Paste each loop's resume prompt** from the repo's README.md into
  its tab.
- **Schedule per-loop ticks** ("every 10m", "daily 09:00 America/Los_Angeles",
  "weekly Mon 08:00 America/Los_Angeles", or "none").
- **Health-check every 60s.** A killed pane triggers an automatic respawn
  with the resume prompt re-pasted; persistent failure surfaces as
  `failed-recreate` in the TUI.
- **Survive daemon restarts.** Each spawned konsole carries a UUID env
  var so a re-launched daemon re-adopts existing windows instead of
  spawning duplicates. Loops that were live before the daemon went down
  (crash, reboot, binary upgrade) are **restored on startup** — any whose
  window didn't survive is respawned with its resume prompt re-pasted.
  Loops you never brought up, or explicitly `down`ed/closed, stay down, so
  a restart never opens windows you didn't ask for.
- **TUI** with arrow-key navigation, force-tick (`t`), focus
  (`a`), close (`c`), update prompt hint (`u`), move-group (`g`),
  load-repo / persist (`p`), and recover-now (`R`).

## Build

Requires a recent stable Rust toolchain (1.95+) and KDE Plasma 5/6 on X11.

```bash
git clone <repo-url> ~/loopctl
cd ~/loopctl
cargo build --release
```

The binary lands at `~/loopctl/target/release/loopctl`. Add
that to PATH or symlink:

```bash
ln -s ~/loopctl/target/release/loopctl ~/.local/bin/loopctl
```

Run-time deps: `konsole`, `git`, `python3`, `journalctl --user`.

## First run

```bash
# Point loopctl at a claude-loops repo (clones into
# ~/.local/share/loopctl/repos/<basename> if a URL):
loopctl load-repo https://github.com/example-org/example-loops

# Or set the path explicitly:
loopctl set-repo ~/claude-loops

# Bring up the daemon (auto-spawns on bare invocation):
loopctl

# Open the TUI:
loopctl tui
```

`load-repo` discovers loops from `<repo>/README.md`, generates a
`workbench.toml` if missing, packs loops into the first empty virtual
desktops, and (re)starts the daemon.

## Subcommands

| Command | What it does |
|---|---|
| `loopctl` | Spawn the daemon detached if it's not already running. |
| `loopctl init` | Validate workbench.toml without side effects. |
| `loopctl daemon --foreground` | Run the long-lived daemon in the foreground. |
| `loopctl daemon-stop` | Graceful shutdown via RPC. |
| `loopctl up` | Bring all configured loops up. |
| `loopctl down` | Close all non-central loop panes. |
| `loopctl tick <name>` | Force-fire a loop's tick now. |
| `loopctl recover` | Force a pane health-check + recovery pass. |
| `loopctl reload` | Re-read workbench.toml in the running daemon. |
| `loopctl status [--json]` | Dump current state. |
| `loopctl tui` | ratatui-based dashboard. |
| `loopctl attach <name>` | Raise a loop's tab. |
| `loopctl add-loop --name <N> --group <G> (--message <STR> \| --prompt-file <PATH>) ...` | Append a loop to README.md + workbench.toml. New loops opt out of `--dangerously-skip-permissions` until you trust them. |
| `loopctl close <name>` / `reopen <name>` | Soft-disable / re-enable a loop. |
| `loopctl update-prompt <name> --message <STR> [--repaste]` | Replace a loop's README block. |
| `loopctl move-group <name> <new>` | (Via TUI `g`) move a loop to a different virtual desktop. |
| `loopctl load-repo <path-or-url>` | Resolve, discover loops, write workbench.toml, save user config, restart daemon. |
| `loopctl set-repo <path>` | Save the active claude_loops_dir without re-discovering. |

## TUI shortcuts

```
↑/↓  select   t  tick   a  attach   g  move-group   c  close
u  update-prompt   p  persist (load repo)   n  add-loop hint
R  recover   r  refresh   q  quit
```

## Config

User config: `~/.config/loopctl/config.toml`

```toml
claude_loops_dir = "~/claude-loops"
```

Workbench config (per repo): `<claude_loops_dir>/workbench.toml`

```toml
schema_version = 1

[workbench]
claude_loops_dir = "~/claude-loops"
terminal         = "konsole"
central_group    = "8"
default_dispatch = "tick_script"
default_agent    = "claude"                             # agent a loop uses if it sets none
agent_args       = ["--dangerously-skip-permissions"]   # default-agent args (alias: claude_args)
paste_submit_delay_ms = 700                              # bump if Enter doesn't auto-fire

[terminal.konsole]
profile = "ClaudeLoop"   # optional Konsole profile name

# Per-agent overrides. Built-in presets exist for `claude`, `codex`, and
# `opencode` (binary + hands-off flag), so these tables are only needed to
# change a binary/args or to define a brand-new custom agent.
[agents.codex]
binary = "codex"
args   = ["--dangerously-bypass-approvals-and-sandbox"]

[[loops]]
name             = "alpha-loop"
dir              = "alpha-loop"
readme_anchor    = "alpha-loop--example-tracker"
group_key        = "7"           # X11 virtual desktop number
schedule         = "every 10m"
dispatch         = "tick_script" # or "resume_prompt", or "none"
tick_script      = "skill/scripts/tick.py"
tick_args        = ["--cache", "$HOME/.claude/changecase-tracker-cache/cases.json"]
# agent          = "codex"       # claude | codex | opencode | custom; omit = default_agent
# Optional per-loop override of agent_args. `[]` = stock agent with prompts.
# agent_args = []
```

**Agents.** Each loop launches a coding-agent CLI in its pane — `claude`
by default, or `codex` / `opencode` (or any custom agent from `[agents.*]`)
via the per-loop `agent` field. Built-in presets supply each agent's binary
and hands-off flag; `[agents.<name>]` overrides them or defines new agents.
The older `claude_binary` / `claude_args` keys still parse (they're aliases
for `agent_binary` / `agent_args`) and are rewritten to the new names on
first daemon load.

State (rebuildable, kept out of the workbench repo):
`~/.local/state/agent-loop-workbench/state.json`

Daemon socket: `$XDG_RUNTIME_DIR/agent-loop-workbench.sock`

Logs: `~/.local/state/agent-loop-workbench/loopctl.log`

## Architecture

```
src/
  main.rs                  # clap entrypoint
  config/                  # workbench.toml + user config schemas
  daemon/                  # tokio runtime, IPC, scheduler, lock file
    ipc.rs                 # JSON-RPC over Unix socket
  orchestrator.rs          # up / down / move-group / close / update-prompt
  dispatch.rs              # per-loop tick (tick_script | resume_prompt | none)
  scheduler.rs             # next_fire(schedule, now, last_fired)
  metrics/                 # heuristic markdown parsing for tracker.md
  readme/                  # GFM-slugged anchor → fenced block extraction
  discover.rs              # walk README.md → discovered loops
  repo_loader.rs           # local path or git clone → workbench.toml
  state/                   # persistent state model + atomic writer
  terminal/                # plugin trait + impls
    mod.rs                 # TerminalPlugin trait, PaneSpec, PaneId
    mock.rs                # in-memory plugin for tests
    konsole/               # KDE Plasma X11 (Konsole + KWin Scripting via D-Bus)
    tmux.rs                # stub
    mac.rs                 # stub
  tui/                     # ratatui dashboard + input modes
  util/                    # XDG paths, tilde expansion
```

## Testing

```bash
cargo test                                                  # 100+ unit + integration tests
cargo test --test konsole_smoke -- --ignored --test-threads=1   # live KDE smoke
cargo clippy --all-targets -- -D warnings
```

The Konsole smoke tests are `#[ignore]` because they require a live KDE
session. Other tests run against `MockTerminalPlugin`.

## License & dependencies

`loopctl` is released under the [Apache License 2.0](LICENSE.txt). See
[NOTICE.txt](NOTICE.txt) for required attribution.

Third-party dependencies and their licenses are listed in
[THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md), regenerated from
`Cargo.lock` via:

```bash
cargo install cargo-about --features cli
cargo about generate about.hbs -o THIRD_PARTY_NOTICES.md
```
