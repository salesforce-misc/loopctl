// Copyright (c) 2026, Salesforce, Inc.
// SPDX-License-Identifier: Apache-2.0
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! `loopctl` — thin CLI that talks to a long-lived daemon.
//!
//! `loopctl daemon [--foreground]` runs the daemon. Every other subcommand
//! sends a JSON-RPC request to the daemon's Unix socket and prints the
//! response.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

use agent_loop_workbench::{
    config,
    daemon::{self, ipc},
    terminal::{iterm2::Iterm2Plugin, konsole::KonsolePlugin, TerminalPlugin},
    util::paths,
};

#[derive(Parser, Debug)]
#[command(name = "loopctl", version, about = "Manage Claude agent loops")]
struct Cli {
    /// Path to workbench.toml. Defaults to ~/claude-loops/workbench.toml.
    #[arg(long, global = true)]
    config: Option<PathBuf>,

    /// IPC socket override. Defaults to $XDG_RUNTIME_DIR/agent-loop-workbench.sock.
    #[arg(long, global = true)]
    socket: Option<PathBuf>,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand, Debug, Clone)]
enum Command {
    /// Validate workbench.toml without side effects.
    Init,
    /// Run the long-lived daemon (foreground).
    Daemon {
        #[arg(long, default_value_t = true)]
        foreground: bool,
    },
    /// Bring all loops up.
    Up,
    /// Close all non-central loop panes.
    Down,
    /// Force a tick for one loop now.
    Tick { name: String },
    /// Print daemon status.
    Status {
        #[arg(long)]
        json: bool,
    },
    /// Focus a loop's pane (KDE switches desktop).
    Attach { name: String },
    /// Stop the running daemon.
    DaemonStop,
    /// Open the workbench TUI.
    Tui,
    /// Force a pane health check + recovery pass.
    Recover,
    /// Re-read workbench.toml in the running daemon.
    Reload,
    /// Append a new loop to workbench.toml + README.md.
    ///
    /// Requires --name, --group, and exactly one of --message or
    /// --prompt-file. The clap ArgGroup below makes the prompt-source
    /// requirement appear in the auto-generated usage string so it's
    /// visible in error messages, not just `--help`.
    #[command(group(
        clap::ArgGroup::new("prompt_source")
            .required(true)
            .multiple(false)
            .args(["message", "prompt_file"])
    ))]
    AddLoop {
        /// Loop name (also the default tab title and dir name).
        #[arg(long)]
        name: String,
        /// Path of the loop's workdir, relative to claude_loops_dir.
        /// Defaults to `name`.
        #[arg(long)]
        dir: Option<String>,
        /// Group key — for the Konsole plugin, the X11 virtual desktop number.
        #[arg(long)]
        group: String,
        /// Schedule. Examples: `none`, `every 10m`, `daily 09:00 America/Los_Angeles`.
        #[arg(long, default_value = "none")]
        schedule: String,
        /// Heading text used in README.md and slugified into readme_anchor.
        /// Defaults to `<name> — workbench loop`.
        #[arg(long)]
        heading: Option<String>,
        /// Inline resume prompt body. Required iff --prompt-file is absent.
        #[arg(long)]
        message: Option<String>,
        /// Resume prompt body from file. Required iff --message is absent.
        #[arg(long)]
        prompt_file: Option<PathBuf>,
        /// dispatch kind: resume_prompt, tick_script, or none.
        #[arg(long, default_value = "resume_prompt")]
        dispatch: String,
        /// Required iff dispatch=tick_script. Path relative to dir.
        #[arg(long)]
        tick_script: Option<String>,
        /// Skip the post-add `up` RPC. By default `add-loop` spawns the
        /// new loop's pane and pastes the resume prompt right away;
        /// pass `--no-up` to add to the config but defer spawning until
        /// the next `loopctl up`.
        #[arg(long, default_value_t = false)]
        no_up: bool,
    },
    /// Set or print the user-level config (~/.config/loopctl/config.toml).
    SetRepo {
        /// Path to the claude_loops_dir. If omitted, prints current value.
        path: Option<String>,
    },
    /// Load a claude-loops repo: clone if URL, discover loops from
    /// README.md, generate workbench.toml on first load, save the
    /// resolved path to user config, restart the daemon.
    LoadRepo {
        /// Local path or git URL.
        repo: String,
    },
    /// Stop the running daemon (if any) and spawn a fresh one detached.
    /// Useful after upgrading the binary or rotating workbench.toml.
    Restart {
        /// Also call `up` after the new daemon binds its socket so all
        /// configured loops get panes immediately.
        #[arg(long, default_value_t = false)]
        up: bool,
    },
    /// Permanently remove a loop. **Destructive**: closes the live
    /// pane, drops the entry from workbench.toml, strips the README
    /// block, and removes the loop directory recursively. Prompts for
    /// confirmation by default; pass --yes to skip.
    DeleteLoop {
        name: String,
        /// Skip the confirmation prompt. Use only in scripts where the
        /// caller has already verified the name.
        #[arg(long, default_value_t = false)]
        yes: bool,
    },
    /// Mark a loop inactive: stop scheduling, close its pane.
    Close { name: String },
    /// Reactivate a closed loop and bring up its pane.
    Reopen { name: String },
    /// Replace a loop's resume prompt body in the README.
    ///
    /// Requires the loop name plus exactly one of --message or
    /// --prompt-file (enforced by clap so the missing-args error
    /// spells out the choice).
    #[command(group(
        clap::ArgGroup::new("update_prompt_source")
            .required(true)
            .multiple(false)
            .args(["message", "prompt_file"])
    ))]
    UpdatePrompt {
        name: String,
        /// Inline new body. Required iff --prompt-file is absent.
        #[arg(long)]
        message: Option<String>,
        /// Read new body from file. Required iff --message is absent.
        #[arg(long)]
        prompt_file: Option<PathBuf>,
        /// Also paste the new prompt into the loop's running pane.
        #[arg(long, default_value_t = false)]
        repaste: bool,
    },
}

fn default_config_path() -> Result<PathBuf> {
    let home = std::env::var("HOME").context("HOME not set")?;
    Ok(PathBuf::from(home).join("claude-loops/workbench.toml"))
}

fn resolve_socket(cli: &Cli) -> Result<PathBuf> {
    if let Some(p) = cli.socket.as_ref() {
        return Ok(p.clone());
    }
    paths::ipc_socket()
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_target(false)
        .init();

    let cli = Cli::parse();

    let Some(cmd) = cli.command.clone() else {
        return cmd_default(&cli).await;
    };
    match &cmd {
        Command::Init => cmd_init(&cli),
        Command::Daemon { foreground: _ } => cmd_daemon(&cli).await,
        Command::Up => cmd_simple_rpc(&cli, "up", serde_json::Value::Null).await,
        Command::Down => cmd_simple_rpc(&cli, "down", serde_json::Value::Null).await,
        Command::Tick { name } => {
            cmd_simple_rpc(&cli, "tick", serde_json::json!({"name": name})).await
        }
        Command::Status { json } => cmd_status(&cli, *json).await,
        Command::Attach { name } => {
            cmd_simple_rpc(&cli, "attach", serde_json::json!({"name": name})).await
        }
        Command::DaemonStop => cmd_simple_rpc(&cli, "shutdown", serde_json::Value::Null).await,
        Command::Tui => cmd_tui(&cli).await,
        Command::Recover => cmd_simple_rpc(&cli, "recover", serde_json::Value::Null).await,
        Command::Reload => cmd_simple_rpc(&cli, "reload", serde_json::Value::Null).await,
        Command::AddLoop {
            name,
            dir,
            group,
            schedule,
            heading,
            message,
            prompt_file,
            dispatch,
            tick_script,
            no_up,
        } => {
            cmd_add_loop(
                &cli,
                name,
                dir.as_deref(),
                group,
                schedule,
                heading.as_deref(),
                message.as_deref(),
                prompt_file.as_deref(),
                dispatch,
                tick_script.as_deref(),
                !*no_up,
            )
            .await
        }
        Command::SetRepo { path } => cmd_set_repo(path.as_deref()),
        Command::LoadRepo { repo } => cmd_load_repo(&cli, repo).await,
        Command::Restart { up } => cmd_restart(&cli, *up).await,
        Command::DeleteLoop { name, yes } => cmd_delete_loop(&cli, name, *yes).await,
        Command::Close { name } => {
            cmd_simple_rpc(&cli, "close-loop", serde_json::json!({"name": name})).await
        }
        Command::Reopen { name } => {
            cmd_simple_rpc(&cli, "reopen-loop", serde_json::json!({"name": name})).await
        }
        Command::UpdatePrompt {
            name,
            message,
            prompt_file,
            repaste,
        } => cmd_update_prompt(&cli, name, message.as_deref(), prompt_file.as_deref(), *repaste).await,
    }
}

async fn cmd_update_prompt(
    cli: &Cli,
    name: &str,
    message: Option<&str>,
    prompt_file: Option<&std::path::Path>,
    repaste: bool,
) -> Result<()> {
    if message.is_some() && prompt_file.is_some() {
        anyhow::bail!("--message and --prompt-file are mutually exclusive");
    }
    let body = match (message, prompt_file) {
        (Some(m), _) => m.to_string(),
        (None, Some(p)) => std::fs::read_to_string(p)
            .with_context(|| format!("read --prompt-file {p:?}"))?,
        (None, None) => anyhow::bail!("must supply --message or --prompt-file"),
    };
    cmd_simple_rpc(
        cli,
        "update-prompt",
        serde_json::json!({"name": name, "body": body, "repaste": repaste}),
    )
    .await
}

/// `loopctl` with no subcommand. If the daemon is already running, tell
/// the user how to proceed; otherwise validate config, spawn a detached
/// daemon, wait for it to come up, and report success or the failure.
async fn cmd_default(cli: &Cli) -> Result<()> {
    let socket = resolve_socket(cli)?;
    if daemon_listening(&socket).await {
        println!(
            "loopctl daemon is already running (socket: {}).\n\nNext steps:\n  loopctl tui      — open the workbench TUI\n  loopctl status   — text snapshot\n  loopctl --help   — full command list",
            socket.display()
        );
        return Ok(());
    }
    // A socket file with no daemon behind it (crashed/killed daemon) would
    // otherwise make the readiness wait below return instantly against the
    // stale path. Reclaim it now; the fresh daemon's bind_socket also clears
    // it, but removing here keeps `while !socket.exists()` honest.
    if socket.exists() {
        if let Err(e) = std::fs::remove_file(&socket) {
            eprintln!(
                "note: could not remove stale socket {}: {e}",
                socket.display()
            );
        }
    }
    // Pre-flight: validate the workbench.toml resolution + parse before
    // we spawn anything. A bad user config would otherwise produce a
    // dead daemon that we then fruitlessly poll for. The error message
    // mirrors what `loopctl init` would report.
    let cfg_path = resolve_config_path(cli)?;
    if !cfg_path.exists() {
        anyhow::bail!(
            "workbench.toml not found at {}\n\nFix one of:\n  loopctl set-repo ~/claude-loops    # point at a different repo\n  loopctl load-repo <path-or-url>    # clone/load a fresh repo",
            cfg_path.display()
        );
    }
    config::load(&cfg_path)
        .with_context(|| format!("validate workbench.toml at {}", cfg_path.display()))?;

    let log_path = paths::log_file()?;
    let pid = spawn_detached_daemon(cli, &log_path)?;

    // Wait for the daemon to become reachable. If it dies before binding
    // the socket, surface the tail of the log so the user sees why.
    let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(5);
    while !socket.exists() {
        if tokio::time::Instant::now() >= deadline {
            let tail = read_log_tail(&log_path, 30);
            anyhow::bail!(
                "daemon spawned (pid {pid}) but never bound its socket within 5s\n--- last lines of {} ---\n{tail}",
                log_path.display()
            );
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(80)).await;
    }
    println!(
        "started loopctl daemon (pid {pid}, log: {}).\n\nNext steps:\n  loopctl tui      — open the workbench TUI\n  loopctl status   — text snapshot\n  loopctl --help   — full command list",
        log_path.display()
    );
    Ok(())
}

/// `loopctl delete-loop <name>` — destructive removal of a loop.
/// Prompts before doing anything; `--yes` skips the prompt.
async fn cmd_delete_loop(cli: &Cli, name: &str, yes: bool) -> Result<()> {
    let cfg_path = resolve_config_path(cli)?;
    let cfg = config::load(&cfg_path)
        .with_context(|| format!("validate workbench.toml at {}", cfg_path.display()))?;
    let lc = cfg
        .loop_by_name(name)
        .ok_or_else(|| anyhow::anyhow!("unknown loop `{name}`"))?;

    let loops_dir =
        std::path::PathBuf::from(agent_loop_workbench::util::expand_path(
            &cfg.workbench.claude_loops_dir,
        ));
    let loop_dir_full = loops_dir.join(&lc.dir);
    let readme_path = loops_dir.join("README.md");

    eprintln!("About to DELETE loop `{name}`. This will:");
    eprintln!(
        "  1. close any live pane (state will lose pane_id={:?})",
        // We don't have state here; lookup is best-effort via status RPC.
        // Skip detail to keep this branch small.
        "<see daemon state>"
    );
    eprintln!(
        "  2. remove the [[loops]] entry from {}",
        cfg_path.display()
    );
    eprintln!(
        "  3. strip the `### {}` block from {}",
        lc.readme_anchor,
        readme_path.display()
    );
    eprintln!(
        "  4. recursively delete the loop directory: {}",
        loop_dir_full.display()
    );
    eprintln!();
    eprintln!("This action is NOT reversible without a git restore.");

    if !yes {
        eprint!("Type the loop name `{name}` to confirm (anything else cancels): ");
        use std::io::{self, BufRead, Write};
        io::stderr().flush().ok();
        let stdin = io::stdin();
        let mut line = String::new();
        stdin
            .lock()
            .read_line(&mut line)
            .context("read confirmation from stdin")?;
        if line.trim() != name {
            eprintln!("cancelled — input did not match `{name}`");
            return Ok(());
        }
    }

    // Daemon-side delete (it owns config + state mutation).
    let socket = resolve_socket(cli)?;
    if !socket.exists() {
        anyhow::bail!(
            "daemon is not running; start it (`loopctl daemon` or just `loopctl`) and re-run delete-loop"
        );
    }
    let resp = daemon::ipc::call_once(
        &socket,
        &daemon::ipc::Request {
            method: "delete-loop".into(),
            params: serde_json::json!({"name": name}),
        },
    )
    .await
    .context("delete-loop RPC")?;
    match resp {
        daemon::ipc::Response::Ok { result, .. } => {
            println!("deleted `{name}`:");
            for (label, key) in [
                ("pane_closed", "pane_closed"),
                ("removed_from_config", "removed_from_config"),
                ("readme_block_stripped", "readme_block_stripped"),
                ("state_dropped", "state_dropped"),
                ("dir_removed", "dir_removed"),
            ] {
                println!("  {label}: {}", result.get(key).cloned().unwrap_or(serde_json::Value::Null));
            }
        }
        daemon::ipc::Response::Err { error, .. } => {
            anyhow::bail!("delete-loop failed: {error}");
        }
    }
    Ok(())
}

/// `loopctl restart` — graceful daemon shutdown + spawn a fresh detached
/// daemon from `current_exe()`. The fresh daemon picks up whatever binary
/// invoked `restart`, so this is also how you "reload after rebuild."
/// Probe whether a daemon is actually listening on `socket`, as opposed
/// to a stale socket file left behind by a crashed or killed daemon.
/// A successful connect means something is accepting connections;
/// ECONNREFUSED (or a missing file) means the path is dead and can be
/// reclaimed. Checking `socket.exists()` alone is not enough — the file
/// outlives the process that created it.
async fn daemon_listening(socket: &std::path::Path) -> bool {
    if !socket.exists() {
        return false;
    }
    tokio::net::UnixStream::connect(socket).await.is_ok()
}

/// Ask the running daemon for its compiled version string. Returns
/// `None` if no daemon is reachable (socket missing, RPC error, or the
/// running daemon predates the `version` method).
async fn query_daemon_version(socket: &std::path::Path) -> Option<String> {
    if !socket.exists() {
        return None;
    }
    let resp = daemon::ipc::call_once(
        socket,
        &daemon::ipc::Request {
            method: "version".into(),
            params: serde_json::Value::Null,
        },
    )
    .await
    .ok()?;
    match resp {
        daemon::ipc::Response::Ok { result, .. } => result
            .get("version")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        daemon::ipc::Response::Err { .. } => None,
    }
}

async fn cmd_restart(cli: &Cli, also_up: bool) -> Result<()> {
    let socket = resolve_socket(cli)?;

    // 1. Pre-flight the config so we don't tear down a working daemon
    //    only to find we can't bring up a new one. Same logic as the
    //    bare-loopctl path so users don't get inconsistent behavior.
    let cfg_path = resolve_config_path(cli)?;
    if !cfg_path.exists() {
        anyhow::bail!(
            "workbench.toml not found at {}\n\nFix one of:\n  loopctl set-repo ~/claude-loops\n  loopctl load-repo <path-or-url>",
            cfg_path.display()
        );
    }
    config::load(&cfg_path)
        .with_context(|| format!("validate workbench.toml at {}", cfg_path.display()))?;

    // 1b. Capture the OLD daemon's version string before we tell it to
    //     shut down. The user wants to know whether `restart` actually
    //     swapped in newer code, not just bounced the same daemon.
    let old_version = query_daemon_version(&socket).await;

    // 2. Best-effort graceful shutdown via RPC. If no daemon's running,
    //    that's fine — we still proceed to spawn.
    if socket.exists() {
        match daemon::ipc::call_once(
            &socket,
            &daemon::ipc::Request {
                method: "shutdown".into(),
                params: serde_json::Value::Null,
            },
        )
        .await
        {
            Ok(_) => {}
            Err(e) => eprintln!("note: shutdown RPC failed ({e:#}); proceeding anyway"),
        }
        // Wait for the daemon's own cleanup to remove the socket.
        let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(5);
        while socket.exists() {
            if tokio::time::Instant::now() >= deadline {
                eprintln!(
                    "warning: previous daemon socket did not vanish within 5s; the new daemon may fail to bind"
                );
                break;
            }
            tokio::time::sleep(tokio::time::Duration::from_millis(80)).await;
        }
    }

    // 3. Spawn the fresh daemon. `spawn_detached_daemon` uses
    //    `current_exe()`, so this is the same binary that's running
    //    `restart` right now — handy after a rebuild.
    let log_path = paths::log_file()?;
    let pid = spawn_detached_daemon(cli, &log_path)?;
    if !wait_for_socket(&socket, 5).await {
        let tail = read_log_tail(&log_path, 30);
        anyhow::bail!(
            "daemon spawned (pid {pid}) but never bound its socket within 5s\n--- last lines of {} ---\n{tail}",
            log_path.display()
        );
    }

    let exe = std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "<unknown>".into());
    println!("daemon restarted (pid {pid}, binary: {exe}, log: {})", log_path.display());

    // Compare the new daemon's version against the old one's so the
    // user can tell at a glance whether the restart actually picked up
    // newer code (vs bouncing the same binary).
    let new_version = query_daemon_version(&socket).await;
    match (old_version.as_deref(), new_version.as_deref()) {
        (Some(o), Some(n)) if o == n => {
            println!("loopctl restart: {o} (unchanged)");
        }
        (Some(o), Some(n)) => {
            println!("loopctl restart: {o} -> {n}");
        }
        (None, Some(n)) => {
            println!("loopctl restart: (no prior daemon) -> {n}");
        }
        (_, None) => {
            eprintln!("note: could not query the new daemon's version RPC");
        }
    }

    // 4. Optional: bring panes back so the user doesn't have to chase
    //    `loopctl up` themselves.
    if also_up {
        match daemon::ipc::call_once(
            &socket,
            &daemon::ipc::Request {
                method: "up".into(),
                params: serde_json::Value::Null,
            },
        )
        .await
        {
            Ok(daemon::ipc::Response::Ok { .. }) => println!("up fired — every loop has a pane"),
            Ok(daemon::ipc::Response::Err { error, .. }) => {
                eprintln!("warning: `up` failed: {error}\nrun `loopctl up` manually after fixing")
            }
            Err(e) => eprintln!("warning: could not RPC `up`: {e:#}"),
        }
    }
    Ok(())
}

fn read_log_tail(path: &std::path::Path, lines: usize) -> String {
    let body = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => return format!("(could not read log: {e})"),
    };
    let collected: Vec<&str> = body.lines().collect();
    let start = collected.len().saturating_sub(lines);
    collected[start..].join("\n")
}

/// Spawn `loopctl daemon --foreground` as a detached child (setsid).
/// Stdout/stderr go to `log_path`. Returns the child PID.
fn spawn_detached_daemon(cli: &Cli, log_path: &std::path::Path) -> Result<u32> {
    use std::os::unix::process::CommandExt;
    if let Some(parent) = log_path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)
        .with_context(|| format!("open daemon log {log_path:?}"))?;
    let log_err = log.try_clone()?;
    let exe = std::env::current_exe().context("locate loopctl binary")?;

    let mut cmd = std::process::Command::new(&exe);
    cmd.arg("daemon").arg("--foreground");
    if let Some(p) = cli.config.as_ref() {
        cmd.arg("--config").arg(p);
    }
    if let Some(s) = cli.socket.as_ref() {
        cmd.arg("--socket").arg(s);
    }
    cmd.stdin(std::process::Stdio::null())
        .stdout(log)
        .stderr(log_err);
    // setsid so the child survives this process exiting and isn't killed
    // by SIGHUP if invoked from a terminal that closes.
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let child = cmd.spawn().with_context(|| format!("spawn {exe:?}"))?;
    Ok(child.id())
}

async fn cmd_load_repo(cli: &Cli, repo: &str) -> Result<()> {
    use agent_loop_workbench::repo_loader::LoadAction;
    let plugin: Box<dyn TerminalPlugin> = if cfg!(target_os = "macos") {
        Box::new(Iterm2Plugin::new())
    } else {
        Box::new(KonsolePlugin::new(None))
    };
    let outcome =
        agent_loop_workbench::repo_loader::load_repo(repo, plugin.as_ref()).await?;
    match &outcome.action {
        LoadAction::Generated => {
            println!(
                "loaded {} (generated workbench.toml — {} loops):",
                outcome.repo_dir.display(),
                outcome.discovered.len()
            );
            for (name, desk) in &outcome.desktop_plan {
                println!("  {name:<32} → desktop {desk}");
            }
        }
        LoadAction::Appended { added } => {
            println!(
                "loaded {} (appended {added} new loop(s) to existing workbench.toml):",
                outcome.repo_dir.display()
            );
            for (name, desk) in &outcome.desktop_plan {
                println!("  {name:<32} → desktop {desk}");
            }
        }
        LoadAction::UpToDate => {
            println!(
                "loaded {} (workbench.toml already covers every discovered loop)",
                outcome.repo_dir.display()
            );
        }
    }
    println!("user config now points at this repo.");

    // Restart the daemon (it caches config_path at startup; a plain
    // `reload` RPC would re-read the OLD repo).
    let socket = resolve_socket(cli)?;
    let was_running = socket.exists();
    if was_running {
        let _ = daemon::ipc::call_once(
            &socket,
            &daemon::ipc::Request {
                method: "shutdown".into(),
                params: serde_json::Value::Null,
            },
        )
        .await;
        let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(5);
        while socket.exists() {
            if tokio::time::Instant::now() >= deadline {
                eprintln!(
                    "warning: old daemon socket did not vanish; the new daemon may not start cleanly"
                );
                break;
            }
            tokio::time::sleep(tokio::time::Duration::from_millis(80)).await;
        }
    }
    let log_path = paths::log_file()?;
    let pid = spawn_detached_daemon(cli, &log_path)?;
    println!("daemon (re)started: pid {pid} (log: {})", log_path.display());

    // Wait for the new daemon's socket, then ask it to bring all loops
    // up. Without this step, newly-discovered loops would sit `Unknown`
    // until the user explicitly runs `loopctl up`.
    let socket = resolve_socket(cli)?;
    if !wait_for_socket(&socket, 5).await {
        eprintln!("note: daemon hasn't bound its socket yet; run `loopctl up` manually once it's ready");
        return Ok(());
    }
    match daemon::ipc::call_once(
        &socket,
        &daemon::ipc::Request {
            method: "up".into(),
            params: serde_json::Value::Null,
        },
    )
    .await
    {
        Ok(daemon::ipc::Response::Ok { .. }) => {
            println!("loopctl up fired — every loop should now have a pane");
        }
        Ok(daemon::ipc::Response::Err { error, .. }) => {
            eprintln!("warning: `up` returned: {error}\nrun `loopctl up` manually after fixing");
        }
        Err(e) => eprintln!("warning: could not RPC `up`: {e:#}"),
    }
    Ok(())
}

async fn wait_for_socket(socket: &std::path::Path, timeout_secs: u64) -> bool {
    let deadline =
        tokio::time::Instant::now() + tokio::time::Duration::from_secs(timeout_secs);
    while !socket.exists() {
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(80)).await;
    }
    true
}

fn cmd_set_repo(path: Option<&str>) -> Result<()> {
    let mut uc = config::load_user_config().unwrap_or_default();
    match path {
        Some(p) => {
            uc.claude_loops_dir = Some(p.to_string());
            config::save_user_config(&uc)?;
            println!(
                "user config saved at {}\n  claude_loops_dir = {}",
                config::user_config_path()?.display(),
                p
            );
        }
        None => match &uc.claude_loops_dir {
            Some(p) => println!("claude_loops_dir = {p}"),
            None => println!("claude_loops_dir = (unset; defaults to ~/claude-loops)"),
        },
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn cmd_add_loop(
    cli: &Cli,
    name: &str,
    dir: Option<&str>,
    group: &str,
    schedule: &str,
    heading: Option<&str>,
    message: Option<&str>,
    prompt_file: Option<&std::path::Path>,
    dispatch: &str,
    tick_script: Option<&str>,
    auto_up: bool,
) -> Result<()> {
    if message.is_some() && prompt_file.is_some() {
        anyhow::bail!("--message and --prompt-file are mutually exclusive");
    }
    let prompt = match (message, prompt_file) {
        (Some(m), _) => m.to_string(),
        (None, Some(p)) => std::fs::read_to_string(p)
            .with_context(|| format!("read --prompt-file {p:?}"))?,
        (None, None) => {
            anyhow::bail!("must supply --message or --prompt-file")
        }
    };
    let cfg_path = resolve_config_path(cli)?;
    let loops_dir = cfg_path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("config path has no parent: {cfg_path:?}"))?
        .to_path_buf();
    let dir_value = dir.unwrap_or(name).to_string();
    // Reject `..` and absolute paths up front. The same check runs in
    // LoopConfig::validate, but only AFTER the file is written; we
    // need to refuse before mkdir so we can't create a sandbox-escape
    // directory on disk that a later operation could remove.
    {
        let p = std::path::Path::new(&dir_value);
        if p.is_absolute()
            || p.components().any(|c| {
                matches!(
                    c,
                    std::path::Component::ParentDir
                        | std::path::Component::RootDir
                        | std::path::Component::Prefix(_)
                )
            })
        {
            anyhow::bail!(
                "--dir `{dir_value}` must be a relative path with no `..` components"
            );
        }
    }
    // Create the loop's working directory so `up` doesn't error with
    // "workdir does not exist" right after add-loop. The directory is
    // empty by default — any progress.md / tracker.md / scripts the
    // loop wants are the loop owner's responsibility.
    //
    // After mkdir, canonicalize both the loops_dir and the new
    // loop_dir and refuse to continue if the resolved loop path
    // doesn't sit inside the sandbox — this catches the case where a
    // prior symlink in `loops_dir` would have caused the new directory
    // to materialize outside the intended tree.
    let loop_dir_full = loops_dir.join(&dir_value);
    if !loop_dir_full.exists() {
        std::fs::create_dir_all(&loop_dir_full)
            .with_context(|| format!("create loop dir {loop_dir_full:?}"))?;
    }
    if !agent_loop_workbench::orchestrator::is_inside(&loops_dir, &loop_dir_full) {
        anyhow::bail!(
            "loop dir `{}` resolved to a path outside `{}` (symlink trickery?)",
            loop_dir_full.display(),
            loops_dir.display()
        );
    }
    let heading_value = heading
        .map(|h| h.to_string())
        .unwrap_or_else(|| format!("{name} — workbench loop"));
    let dispatch_value = dispatch.to_lowercase();
    if !["resume_prompt", "tick_script", "none"].contains(&dispatch_value.as_str()) {
        anyhow::bail!("--dispatch must be one of: resume_prompt, tick_script, none");
    }
    if dispatch_value == "tick_script" && tick_script.is_none() {
        anyhow::bail!("dispatch = `tick_script` requires --tick-script");
    }
    if dispatch_value != "tick_script" && tick_script.is_some() {
        anyhow::bail!("--tick-script can only be set when --dispatch tick_script");
    }
    // Validate schedule before writing anything.
    use std::str::FromStr;
    config::Schedule::from_str(schedule)
        .with_context(|| format!("invalid --schedule {schedule:?}"))?;

    let anchor = agent_loop_workbench::readme::slugify_heading(&heading_value);
    add_to_readme(&loops_dir.join("README.md"), &heading_value, &anchor, &prompt)?;
    add_to_workbench_toml(
        &cfg_path,
        name,
        &dir_value,
        &anchor,
        group,
        schedule,
        &dispatch_value,
        tick_script,
    )?;
    // Re-validate the freshly-written config; bail loudly if we broke it.
    config::load(&cfg_path).context("workbench.toml failed to re-parse after add-loop")?;

    println!("added loop `{name}` to {}", cfg_path.display());
    println!("  heading: {heading_value}");
    println!("  anchor:  {anchor}");
    println!("  group:   {group}");
    println!("  schedule:{schedule}");
    // If the daemon's running, kick it.
    let socket = resolve_socket(cli)?;
    if !socket.exists() {
        println!("(daemon not running; start it with `loopctl daemon` to load the new loop)");
        return Ok(());
    }
    match daemon::ipc::call_once(
        &socket,
        &daemon::ipc::Request {
            method: "reload".into(),
            params: serde_json::Value::Null,
        },
    )
    .await
    {
        Ok(_) => println!("daemon reloaded"),
        Err(e) => {
            eprintln!("daemon reload failed (you can run `loopctl reload` later): {e}");
            return Ok(());
        }
    }

    if !auto_up {
        println!(
            "(skipped auto-spawn; run `loopctl up` when ready to bring `{name}` to a pane)"
        );
        return Ok(());
    }

    // Spawn the loop's pane and paste its resume prompt. Without this
    // step the loop sits in `Unknown` state because health-check only
    // recovers panes that previously existed, and `up` (full sweep)
    // is the only path that brings a brand-new loop to life.
    match daemon::ipc::call_once(
        &socket,
        &daemon::ipc::Request {
            method: "up".into(),
            params: serde_json::Value::Null,
        },
    )
    .await
    {
        Ok(daemon::ipc::Response::Ok { .. }) => {
            println!("up fired — `{name}` should now have a konsole tab");
        }
        Ok(daemon::ipc::Response::Err { error, .. }) => {
            eprintln!(
                "warning: `up` returned: {error}\nrun `loopctl up` after fixing, or rerun add-loop with --no-up"
            );
        }
        Err(e) => eprintln!("warning: could not RPC `up`: {e:#}"),
    }
    Ok(())
}

fn add_to_readme(
    path: &std::path::Path,
    heading: &str,
    anchor: &str,
    body: &str,
) -> Result<()> {
    let mut existing =
        std::fs::read_to_string(path).unwrap_or_else(|_| String::from("# claude-loops\n"));
    if existing.contains(&format!("### {heading}\n")) {
        anyhow::bail!("README already has a heading `### {heading}`; pick a different one");
    }
    if !existing.ends_with('\n') {
        existing.push('\n');
    }
    if !existing.ends_with("\n\n") {
        existing.push('\n');
    }
    existing.push_str("---\n\n");
    existing.push_str(&format!("### {heading}\n\n"));
    let trimmed = body.trim_end_matches('\n');
    existing.push_str("```\n");
    existing.push_str(trimmed);
    existing.push_str("\n```\n");

    // Sanity check: extracting the same anchor we just wrote should succeed.
    agent_loop_workbench::readme::extract_resume_prompt(&existing, anchor)
        .with_context(|| format!("self-test: anchor `{anchor}` did not parse back"))?;

    std::fs::write(path, existing).with_context(|| format!("write README at {path:?}"))?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn add_to_workbench_toml(
    path: &std::path::Path,
    name: &str,
    dir: &str,
    anchor: &str,
    group: &str,
    schedule: &str,
    dispatch: &str,
    tick_script: Option<&str>,
) -> Result<()> {
    let mut existing = std::fs::read_to_string(path)
        .with_context(|| format!("read workbench.toml at {path:?}"))?;
    if !existing.ends_with('\n') {
        existing.push('\n');
    }
    if !existing.ends_with("\n\n") {
        existing.push('\n');
    }
    existing.push_str(&format!(
        "[[loops]]\nname             = \"{name}\"\ndir              = \"{dir}\"\nreadme_anchor    = \"{anchor}\"\ngroup_key        = \"{group}\"\nschedule         = \"{schedule}\"\ndispatch         = \"{dispatch}\"\n",
    ));
    if let Some(t) = tick_script {
        existing.push_str(&format!("tick_script      = \"{t}\"\n"));
    }
    // New loops get an explicit empty claude_args so they DON'T inherit
    // the workbench-wide --dangerously-skip-permissions default. The
    // operator can flip this to inherit (delete the line) once the loop
    // has earned hands-off trust.
    existing.push_str("# New loop: stock claude with normal permission prompts.\n");
    existing.push_str("# Delete this line to inherit workbench.claude_args (e.g. --dangerously-skip-permissions).\n");
    existing.push_str("claude_args      = []\n");
    std::fs::write(path, existing).with_context(|| format!("write workbench.toml at {path:?}"))?;
    config::snapshot_in_git(path, &format!("loopctl: add-loop {name}"));
    Ok(())
}

async fn cmd_tui(cli: &Cli) -> Result<()> {
    let socket = resolve_socket(cli)?;
    agent_loop_workbench::tui::run(socket).await
}

fn cmd_init(cli: &Cli) -> Result<()> {
    let path = resolve_config_path(cli)?;
    let cfg = config::load(&path).context("load workbench.toml")?;
    println!(
        "OK: workbench.toml at {} parsed cleanly. {} loop(s):",
        path.display(),
        cfg.loops.len()
    );
    for lc in &cfg.loops {
        println!(
            "  - {:<24} group {} schedule {} dispatch {:?}",
            lc.name, lc.group_key, lc.schedule, lc.dispatch
        );
    }
    Ok(())
}

async fn cmd_daemon(cli: &Cli) -> Result<()> {
    let cfg_path = resolve_config_path(cli)?;
    let cfg = config::load(&cfg_path)?;
    let plugin: Arc<dyn TerminalPlugin> = make_plugin(&cfg);
    let socket = resolve_socket(cli)?;
    let state_path = paths::state_file()?;
    let _lock = daemon::acquire_lock(&socket).context("acquire daemon lock")?;
    daemon::run(cfg, cfg_path, plugin, state_path, socket).await
}

fn make_plugin(cfg: &config::WorkbenchConfig) -> Arc<dyn TerminalPlugin> {
    match cfg.workbench.terminal.as_str() {
        "iterm2" => Arc::new(
            Iterm2Plugin::new()
                .with_paste_submit_delay_ms(cfg.workbench.paste_submit_delay_ms),
        ),
        _ => Arc::new(
            KonsolePlugin::new(cfg.terminal.konsole.profile.clone())
                .with_paste_submit_delay_ms(cfg.workbench.paste_submit_delay_ms),
        ),
    }
}

/// `~/.config/loopctl/workbench.toml` (or `$XDG_CONFIG_HOME/loopctl/workbench.toml`).
/// Canonical location since the 2026-05-14 incident where an in-loop tick
/// session ran `git stash push workbench.toml` against a config that lived
/// inside the loops repo and lost six loop stanzas to an unpopped stash.
fn canonical_config_path() -> Result<PathBuf> {
    if let Ok(s) = std::env::var("XDG_CONFIG_HOME") {
        if !s.is_empty() {
            return Ok(PathBuf::from(s).join("loopctl/workbench.toml"));
        }
    }
    let home = std::env::var("HOME").context("HOME not set")?;
    Ok(PathBuf::from(home).join(".config/loopctl/workbench.toml"))
}

fn resolve_config_path(cli: &Cli) -> Result<PathBuf> {
    if let Some(p) = cli.config.as_ref() {
        return Ok(p.clone());
    }
    // 1) Canonical: ~/.config/loopctl/workbench.toml.
    if let Ok(p) = canonical_config_path() {
        if p.exists() {
            return Ok(p);
        }
    }
    // 2) Legacy: ~/.config/loopctl/config.toml's claude_loops_dir + /workbench.toml.
    //    Kept for back-compat with installs that haven't migrated yet.
    if let Ok(uc) = config::load_user_config() {
        if let Some(dir) = uc.claude_loops_dir.as_deref() {
            let expanded = agent_loop_workbench::util::expand_path(dir);
            return Ok(PathBuf::from(expanded).join("workbench.toml"));
        }
    }
    // 3) Legacy: ~/claude-loops/workbench.toml.
    default_config_path()
}

async fn cmd_simple_rpc(cli: &Cli, method: &str, params: serde_json::Value) -> Result<()> {
    let socket = resolve_socket(cli)?;
    let resp = ipc::call_once(
        &socket,
        &ipc::Request {
            method: method.into(),
            params,
        },
    )
    .await
    .with_context(|| format!("rpc {method} (is the daemon running?)"))?;
    print_response(&resp);
    Ok(())
}

async fn cmd_status(cli: &Cli, as_json: bool) -> Result<()> {
    let socket = resolve_socket(cli)?;
    let resp = ipc::call_once(
        &socket,
        &ipc::Request {
            method: "status".into(),
            params: serde_json::Value::Null,
        },
    )
    .await
    .context("rpc status (is the daemon running?)")?;
    let value = match resp {
        ipc::Response::Ok { result, .. } => result,
        ipc::Response::Err { error, .. } => {
            eprintln!("error: {error}");
            std::process::exit(1);
        }
    };
    if as_json {
        println!("{}", serde_json::to_string_pretty(&value)?);
        return Ok(());
    }
    let s: ipc::StatusResponse = serde_json::from_value(value)?;
    println!("terminal: {}", s.terminal);
    println!("loops: {}", s.loop_count);
    for l in &s.loops {
        println!(
            "  {:<22} group={} pane={} last_tick={} next_tick={}",
            l.name,
            l.group_key,
            l.pane_id.as_deref().unwrap_or("-"),
            l.last_tick
                .map(|t| t.to_rfc3339())
                .unwrap_or_else(|| "-".into()),
            l.next_tick
                .map(|t| t.to_rfc3339())
                .unwrap_or_else(|| "-".into()),
        );
    }
    Ok(())
}

fn print_response(resp: &ipc::Response) {
    match resp {
        ipc::Response::Ok { result, .. } => {
            println!("{}", serde_json::to_string_pretty(result).unwrap());
        }
        ipc::Response::Err { error, .. } => {
            eprintln!("error: {error}");
            std::process::exit(1);
        }
    }
}