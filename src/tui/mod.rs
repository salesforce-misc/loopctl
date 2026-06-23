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

//! Workbench TUI. Polls the daemon's `status` RPC every 2 s and renders
//! a one-row-per-loop table: name, group, last_tick, next_tick, key
//! metrics. Quit with `q` or Ctrl+C.

pub mod view;

use std::io;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

use crate::daemon::ipc;

const REFRESH_INTERVAL: Duration = Duration::from_secs(2);
const FLASH_TTL_SECS: i64 = 4;

/// Run the TUI against `socket`. Returns when the user quits.
pub async fn run(socket: PathBuf) -> Result<()> {
    enable_raw_mode().context("enable raw mode")?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen).context("enter alt screen")?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend).context("init terminal")?;

    let result = run_loop(&mut terminal, &socket).await;

    // Always restore terminal even on error.
    disable_raw_mode().ok();
    execute!(terminal.backend_mut(), LeaveAlternateScreen).ok();
    terminal.show_cursor().ok();

    result
}

async fn run_loop<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    socket: &std::path::Path,
) -> Result<()> {
    let mut state = view::ViewState::default();
    state.refresh(socket).await;

    let mut last_refresh = tokio::time::Instant::now();
    loop {
        state.clear_flash_if_stale(FLASH_TTL_SECS);
        terminal.draw(|f| view::render(f, &state))?;

        // Poll for input with timeout = remaining time until next refresh.
        let now = tokio::time::Instant::now();
        let elapsed = now.duration_since(last_refresh);
        let until_refresh = REFRESH_INTERVAL.saturating_sub(elapsed);
        let poll_ms = until_refresh.min(Duration::from_millis(100));
        if event::poll(poll_ms)? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    if state.input.is_some() {
                        // Modal: input mode owns the keystroke.
                        if handle_input_key(&mut state, socket, key.code, key.modifiers).await {
                            state.refresh(socket).await;
                            last_refresh = tokio::time::Instant::now();
                        }
                    } else {
                        match key.code {
                            KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                            KeyCode::Char('r') => {
                                state.refresh(socket).await;
                                last_refresh = tokio::time::Instant::now();
                            }
                            KeyCode::Up | KeyCode::Char('k') => state.move_selection(-1),
                            KeyCode::Down | KeyCode::Char('j') => state.move_selection(1),
                            KeyCode::Char('t') => {
                                tick_selected(&mut state, socket).await;
                                state.refresh(socket).await;
                                last_refresh = tokio::time::Instant::now();
                            }
                            KeyCode::Char('a') => {
                                attach_selected(&mut state, socket).await;
                            }
                            KeyCode::Char('g') => {
                                start_move_group(&mut state);
                            }
                            KeyCode::Char('p') => {
                                start_repo_picker(&mut state);
                            }
                            KeyCode::Char('n') => {
                                state.set_flash(
                                    "to add a loop, run: loopctl add-loop --name <S> --group <N> --message '...'  (then press R or wait 60s)",
                                );
                            }
                            KeyCode::Char('c') => {
                                start_close_loop(&mut state);
                            }
                            KeyCode::Char('d') => {
                                start_delete_loop(&mut state);
                            }
                            KeyCode::Char('u') => {
                                let name = state
                                    .selected_name()
                                    .unwrap_or("<name>")
                                    .to_string();
                                state.set_flash(format!(
                                    "to update prompt, run: loopctl update-prompt {name} --message '...' [--repaste]"
                                ));
                            }
                            KeyCode::Char('R') => {
                                recover_now(&mut state, socket).await;
                                state.refresh(socket).await;
                                last_refresh = tokio::time::Instant::now();
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
        if tokio::time::Instant::now().duration_since(last_refresh) >= REFRESH_INTERVAL {
            state.refresh(socket).await;
            last_refresh = tokio::time::Instant::now();
        }
    }
}

async fn tick_selected(state: &mut view::ViewState, socket: &std::path::Path) {
    let Some(name) = state.selected_name().map(str::to_string) else {
        state.set_flash("no loop selected");
        return;
    };
    let req = ipc::Request {
        method: "tick".into(),
        params: serde_json::json!({"name": name}),
    };
    match ipc::call_once(socket, &req).await {
        Ok(ipc::Response::Ok { result, .. }) => {
            let kind = result
                .get("kind")
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            let exit = result
                .get("exit")
                .and_then(|v| v.as_i64())
                .map(|n| n.to_string())
                .unwrap_or_else(|| "—".into());
            state.set_flash(format!("ticked {name}  kind={kind}  exit={exit}"));
        }
        Ok(ipc::Response::Err { error, .. }) => {
            state.set_flash(format!("tick {name} failed: {error}"));
        }
        Err(e) => state.set_flash(format!("rpc tick failed: {e:#}")),
    }
}

async fn recover_now(state: &mut view::ViewState, socket: &std::path::Path) {
    let req = ipc::Request {
        method: "recover".into(),
        params: serde_json::Value::Null,
    };
    match ipc::call_once(socket, &req).await {
        Ok(ipc::Response::Ok { .. }) => state.set_flash("recover pass triggered"),
        Ok(ipc::Response::Err { error, .. }) => {
            state.set_flash(format!("recover failed: {error}"))
        }
        Err(e) => state.set_flash(format!("rpc recover failed: {e:#}")),
    }
}

fn start_move_group(state: &mut view::ViewState) {
    let Some(s) = state.status.as_ref() else {
        state.set_flash("no daemon");
        return;
    };
    let Some(loop_) = s.loops.get(state.selected) else {
        state.set_flash("no loop selected");
        return;
    };
    state.input = Some(view::InputMode::MoveGroup {
        name: loop_.name.clone(),
        current_group: loop_.group_key.clone(),
        buffer: String::new(),
    });
}

fn start_close_loop(state: &mut view::ViewState) {
    let Some(s) = state.status.as_ref() else {
        state.set_flash("no daemon");
        return;
    };
    let Some(loop_) = s.loops.get(state.selected) else {
        state.set_flash("no loop selected");
        return;
    };
    state.input = Some(view::InputMode::ConfirmClose {
        name: loop_.name.clone(),
    });
}

fn start_delete_loop(state: &mut view::ViewState) {
    let Some(s) = state.status.as_ref() else {
        state.set_flash("no daemon");
        return;
    };
    let Some(loop_) = s.loops.get(state.selected) else {
        state.set_flash("no loop selected");
        return;
    };
    state.input = Some(view::InputMode::ConfirmDelete {
        name: loop_.name.clone(),
    });
}

fn start_repo_picker(state: &mut view::ViewState) {
    // Read current user-config to pre-fill.
    let current = crate::config::load_user_config()
        .ok()
        .and_then(|uc| uc.claude_loops_dir)
        .unwrap_or_else(|| "~/claude-loops".to_string());
    state.input = Some(view::InputMode::SetRepo {
        current: current.clone(),
        buffer: current,
    });
}

/// Returns true if the modal action completed and the caller should
/// refresh status from the daemon.
async fn handle_input_key(
    state: &mut view::ViewState,
    socket: &std::path::Path,
    code: KeyCode,
    _mods: KeyModifiers,
) -> bool {
    let Some(input) = state.input.clone() else {
        return false;
    };
    match (input, code) {
        (_, KeyCode::Esc) => {
            state.input = None;
            false
        }
        (view::InputMode::SetRepo { current, mut buffer }, code) => match code {
            KeyCode::Backspace => {
                buffer.pop();
                state.input = Some(view::InputMode::SetRepo { current, buffer });
                false
            }
            KeyCode::Char(c) => {
                buffer.push(c);
                state.input = Some(view::InputMode::SetRepo { current, buffer });
                false
            }
            KeyCode::Enter => {
                let chosen = if buffer.is_empty() { current } else { buffer };
                state.input = None;
                set_repo_and_reload(state, socket, &chosen).await;
                true
            }
            _ => false,
        },
        (view::InputMode::MoveGroup { name, current_group, mut buffer }, code) => match code {
            KeyCode::Backspace => {
                buffer.pop();
                state.input = Some(view::InputMode::MoveGroup {
                    name,
                    current_group,
                    buffer,
                });
                false
            }
            KeyCode::Char(c) => {
                buffer.push(c);
                state.input = Some(view::InputMode::MoveGroup {
                    name,
                    current_group,
                    buffer,
                });
                false
            }
            KeyCode::Enter => {
                state.input = None;
                if buffer.trim().is_empty() {
                    state.set_flash("move-group cancelled (no group entered)");
                    return false;
                }
                submit_move_group(state, socket, &name, &current_group, &buffer, false).await;
                true
            }
            _ => false,
        },
        (view::InputMode::ConfirmMoveGroup { name, from, to }, code) => match code {
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                state.input = None;
                submit_move_group(state, socket, &name, &from, &to, true).await;
                true
            }
            KeyCode::Char('n') | KeyCode::Char('N') => {
                state.input = None;
                state.set_flash(format!("move {name} -> {to} cancelled"));
                false
            }
            _ => false,
        },
        (view::InputMode::ConfirmClose { name }, code) => match code {
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                state.input = None;
                submit_close_loop(state, socket, &name).await;
                true
            }
            KeyCode::Char('n') | KeyCode::Char('N') => {
                state.input = None;
                state.set_flash(format!("close {name} cancelled"));
                false
            }
            _ => false,
        },
        (view::InputMode::ConfirmDelete { name }, code) => match code {
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                state.input = None;
                submit_delete_loop(state, socket, &name).await;
                true
            }
            KeyCode::Char('n') | KeyCode::Char('N') => {
                state.input = None;
                state.set_flash(format!("delete {name} cancelled"));
                false
            }
            _ => false,
        },
    }
}

async fn submit_delete_loop(
    state: &mut view::ViewState,
    socket: &std::path::Path,
    name: &str,
) {
    let req = ipc::Request {
        method: "delete-loop".into(),
        params: serde_json::json!({"name": name}),
    };
    match ipc::call_once(socket, &req).await {
        Ok(ipc::Response::Ok { result, .. }) => {
            let dir_removed = result
                .get("dir_removed")
                .and_then(|v| v.as_str())
                .unwrap_or("(no dir)");
            state.set_flash(format!("deleted {name}; removed {dir_removed}"));
        }
        Ok(ipc::Response::Err { error, .. }) => {
            state.set_flash(format!("delete-loop failed: {error}"))
        }
        Err(e) => state.set_flash(format!("rpc delete-loop failed: {e:#}")),
    }
}

async fn submit_close_loop(
    state: &mut view::ViewState,
    socket: &std::path::Path,
    name: &str,
) {
    let req = ipc::Request {
        method: "close-loop".into(),
        params: serde_json::json!({"name": name}),
    };
    match ipc::call_once(socket, &req).await {
        Ok(ipc::Response::Ok { .. }) => {
            state.set_flash(format!(
                "closed {name} (run `loopctl reopen {name}` to bring it back)"
            ));
        }
        Ok(ipc::Response::Err { error, .. }) => {
            state.set_flash(format!("close-loop failed: {error}"))
        }
        Err(e) => state.set_flash(format!("rpc close-loop failed: {e:#}")),
    }
}

async fn set_repo_and_reload(
    state: &mut view::ViewState,
    socket: &std::path::Path,
    chosen: &str,
) {
    // Use the full load_repo flow: resolve / clone / discover / write
    // workbench.toml / save user config. We instantiate a fresh
    // KonsolePlugin just for the desktop probe — the running daemon's
    // plugin instance is in a different process and we shouldn't poke
    // its KWin probe machinery from here.
    let plugin = crate::terminal::konsole::KonsolePlugin::new(None);
    let outcome =
        match crate::repo_loader::load_repo(chosen, &plugin).await {
            Ok(o) => o,
            Err(e) => {
                state.set_flash(format!("load_repo failed: {e:#}"));
                return;
            }
        };
    if let Err(e) = restart_daemon(socket).await {
        state.set_flash(format!("repo loaded but daemon restart failed: {e:#}"));
        return;
    }
    // After restart, fire `up` so every configured loop (including
    // newly-appended ones) gets a pane. Without this they'd sit
    // `Unknown` until the user manually runs `loopctl up`.
    let up_msg = match ipc::call_once(
        socket,
        &ipc::Request {
            method: "up".into(),
            params: serde_json::Value::Null,
        },
    )
    .await
    {
        Ok(ipc::Response::Ok { .. }) => "panes spawned",
        Ok(ipc::Response::Err { .. }) => "(up failed — see daemon log)",
        Err(_) => "(up RPC failed)",
    };
    use crate::repo_loader::LoadAction;
    let summary = match &outcome.action {
        LoadAction::Generated => format!(
            "loaded {} (generated workbench.toml — {} loops); daemon restarted; {up_msg}",
            outcome.repo_dir.display(),
            outcome.discovered.len()
        ),
        LoadAction::Appended { added } => format!(
            "loaded {} (appended {added} new loop(s)); daemon restarted; {up_msg}",
            outcome.repo_dir.display()
        ),
        LoadAction::UpToDate => format!(
            "loaded {} (workbench.toml up to date); daemon restarted; {up_msg}",
            outcome.repo_dir.display()
        ),
    };
    state.set_flash(summary);
}

/// Stop the running daemon (if any), wait for the socket to vanish,
/// re-spawn `loopctl daemon` detached, wait for the new socket to appear.
async fn restart_daemon(socket: &std::path::Path) -> anyhow::Result<()> {
    use anyhow::Context;
    // 1. Best-effort shutdown if a daemon is up.
    if socket.exists() {
        let _ = ipc::call_once(
            socket,
            &ipc::Request {
                method: "shutdown".into(),
                params: serde_json::Value::Null,
            },
        )
        .await;
        // 2. Wait for the socket file to be removed (daemon's exit path
        // calls `remove_file` before returning).
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        while socket.exists() {
            if tokio::time::Instant::now() >= deadline {
                anyhow::bail!("timed out waiting for old daemon socket to vanish");
            }
            tokio::time::sleep(std::time::Duration::from_millis(80)).await;
        }
    }
    // 3. Spawn a detached `loopctl daemon` child.
    spawn_daemon_detached().context("spawn daemon")?;
    // 4. Wait for the new socket to appear.
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    while !socket.exists() {
        if tokio::time::Instant::now() >= deadline {
            anyhow::bail!("daemon spawned but socket never appeared");
        }
        tokio::time::sleep(std::time::Duration::from_millis(80)).await;
    }
    Ok(())
}

fn spawn_daemon_detached() -> anyhow::Result<()> {
    use anyhow::Context;
    use std::os::unix::process::CommandExt;
    let log_path = crate::util::paths::log_file()?;
    if let Some(parent) = log_path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .with_context(|| format!("open daemon log {log_path:?}"))?;
    let log_err = log.try_clone()?;
    let exe = std::env::current_exe().context("locate loopctl binary")?;
    let mut cmd = std::process::Command::new(&exe);
    cmd.arg("daemon")
        .arg("--foreground")
        .stdin(std::process::Stdio::null())
        .stdout(log)
        .stderr(log_err);
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let _ = cmd.spawn().with_context(|| format!("spawn {exe:?}"))?;
    Ok(())
}

async fn submit_move_group(
    state: &mut view::ViewState,
    socket: &std::path::Path,
    name: &str,
    from: &str,
    to: &str,
    force: bool,
) {
    let req = ipc::Request {
        method: "move-group".into(),
        params: serde_json::json!({
            "name": name,
            "group": to,
            "force": force,
        }),
    };
    match ipc::call_once(socket, &req).await {
        Ok(ipc::Response::Ok { result, .. }) => {
            if result["moved"] == true {
                state.set_flash(format!("moved {name} → group {to}"));
            } else if result["requires_confirm"] == true {
                let n = result["tabs_in_group"].as_u64().unwrap_or(0);
                state.set_flash(format!(
                    "{name} shares its konsole with {n} tabs total — confirm to recreate"
                ));
                state.input = Some(view::InputMode::ConfirmMoveGroup {
                    name: name.to_string(),
                    from: from.to_string(),
                    to: to.to_string(),
                });
            } else {
                state.set_flash(format!("unexpected move-group response: {result}"));
            }
        }
        Ok(ipc::Response::Err { error, .. }) => {
            state.set_flash(format!("move-group failed: {error}"))
        }
        Err(e) => state.set_flash(format!("rpc move-group failed: {e:#}")),
    }
}

async fn attach_selected(state: &mut view::ViewState, socket: &std::path::Path) {
    let Some(name) = state.selected_name().map(str::to_string) else {
        state.set_flash("no loop selected");
        return;
    };
    let req = ipc::Request {
        method: "attach".into(),
        params: serde_json::json!({"name": name}),
    };
    match ipc::call_once(socket, &req).await {
        Ok(ipc::Response::Ok { .. }) => state.set_flash(format!("attached to {name}")),
        Ok(ipc::Response::Err { error, .. }) => {
            state.set_flash(format!("attach {name} failed: {error}"))
        }
        Err(e) => state.set_flash(format!("rpc attach failed: {e:#}")),
    }
}

/// Single-shot helper used both at startup and on each refresh tick.
pub(crate) async fn fetch_status(socket: &std::path::Path) -> Result<ipc::StatusResponse> {
    let req = ipc::Request {
        method: "status".into(),
        params: serde_json::Value::Null,
    };
    let resp = ipc::call_once(socket, &req).await?;
    match resp {
        ipc::Response::Ok { result, .. } => Ok(serde_json::from_value(result)?),
        ipc::Response::Err { error, .. } => Err(anyhow::anyhow!(error)),
    }
}