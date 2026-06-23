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

//! Unix-socket JSON RPC. Line-delimited JSON frames: one request per line
//! from the client, one response per line from the server.
//!
//! Wire format:
//!
//! ```json
//! { "method": "status", "params": null }
//! ```
//!
//! Response:
//!
//! ```json
//! { "ok": true,  "result": <method-specific> }
//! { "ok": false, "error": "human-readable message" }
//! ```

use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::Mutex;

use super::DaemonContext;
use crate::dispatch as dsp;
use crate::orchestrator;
use crate::state::{LoopState, Metrics, PaneHealth};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    pub method: String,
    #[serde(default)]
    pub params: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Response {
    Ok {
        ok: bool, // always true for this variant
        result: serde_json::Value,
    },
    Err {
        ok: bool, // always false
        error: String,
    },
}

impl Response {
    pub fn ok(value: serde_json::Value) -> Self {
        Self::Ok {
            ok: true,
            result: value,
        }
    }
    pub fn err(message: impl Into<String>) -> Self {
        Self::Err {
            ok: false,
            error: message.into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusResponse {
    pub terminal: String,
    pub loop_count: usize,
    pub loops: Vec<StatusLoop>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusLoop {
    /// Loop name. Older releases called this `slug` on the wire; the
    /// alias keeps any pinned consumer parsing for now.
    #[serde(alias = "slug")]
    pub name: String,
    pub group_key: String,
    pub tab_title: String,
    pub schedule: String,
    pub pane_id: Option<String>,
    pub last_tick: Option<chrono::DateTime<chrono::Utc>>,
    pub next_tick: Option<chrono::DateTime<chrono::Utc>>,
    pub last_dispatch_kind: Option<String>,
    pub last_dispatch_exit: Option<i32>,
    pub metrics: Metrics,
    pub pane_health: PaneHealth,
}

/// Accept loop. Spawns a per-connection task on each accept.
pub async fn serve(listener: UnixListener, ctx: Arc<Mutex<DaemonContext>>) -> Result<()> {
    loop {
        let (stream, _addr) = match listener.accept().await {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(error = %e, "ipc accept failed");
                continue;
            }
        };
        let ctx = ctx.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_connection(stream, ctx).await {
                tracing::warn!(error = %e, "ipc connection error");
            }
        });
    }
}

async fn handle_connection(stream: UnixStream, ctx: Arc<Mutex<DaemonContext>>) -> Result<()> {
    let (read, mut write) = stream.into_split();
    let mut reader = BufReader::new(read);
    let mut line = String::new();
    loop {
        line.clear();
        let n = reader.read_line(&mut line).await?;
        if n == 0 {
            return Ok(());
        }
        let req: Request = match serde_json::from_str(line.trim()) {
            Ok(r) => r,
            Err(e) => {
                send(&mut write, &Response::err(format!("bad request: {e}"))).await?;
                continue;
            }
        };
        let resp = dispatch(&req, &ctx).await;
        send(&mut write, &resp).await?;
    }
}

async fn send(
    w: &mut tokio::net::unix::OwnedWriteHalf,
    resp: &Response,
) -> Result<()> {
    let mut json = serde_json::to_string(resp)?;
    json.push('\n');
    w.write_all(json.as_bytes()).await?;
    w.flush().await?;
    Ok(())
}

/// Read `params.name` (preferred) or `params.slug` (legacy compatibility).
/// Returns the loop name as a String, or None if neither key is present.
fn loop_name_from_params(params: &serde_json::Value) -> Option<String> {
    params
        .get("name")
        .or_else(|| params.get("slug"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

/// RPC method dispatch. Each branch acquires the mutex briefly; long ops
/// (Konsole D-Bus, subprocess) should `tokio::spawn` rather than holding
/// the lock across awaits — that's a step 6/7 concern.
async fn dispatch(req: &Request, ctx: &Arc<Mutex<DaemonContext>>) -> Response {
    match req.method.as_str() {
        "ping" => Response::ok(serde_json::json!({ "pong": true })),
        "version" => version_response(),
        "status" => {
            let g = ctx.lock().await;
            Response::ok(serde_json::to_value(build_status(&g)).unwrap())
        }
        "shutdown" => {
            let g = ctx.lock().await;
            g.shutdown.notify_one();
            Response::ok(serde_json::json!({ "shutdown": true }))
        }
        "reload" => {
            let mut g = ctx.lock().await;
            let path = g.config_path.clone();
            match crate::config::load(&path) {
                Ok(new_cfg) => {
                    let old_count = g.config.loops.len();
                    g.config = new_cfg;
                    let new_count = g.config.loops.len();
                    // Hydrate next_tick for any newly-added loops.
                    let now = chrono::Utc::now();
                    for lc in g.config.loops.clone() {
                        let entry = g.state.loops.entry(lc.name.clone()).or_default();
                        if entry.next_tick.is_none() {
                            if let Ok(s) = lc.parsed_schedule() {
                                entry.next_tick = crate::scheduler::next_fire(
                                    &s,
                                    now,
                                    entry.last_tick,
                                );
                            }
                        }
                    }
                    let _ = g.persist();
                    Response::ok(serde_json::json!({
                        "reloaded": true,
                        "old_loop_count": old_count,
                        "new_loop_count": new_count,
                    }))
                }
                Err(e) => Response::err(format!("reload failed: {e:#}")),
            }
        }
        "up" => {
            let mut g = ctx.lock().await;
            let plugin = g.plugin.clone();
            let cfg = g.config.clone();
            // Pass an owned mutable state to the orchestrator, then put the
            // result back. This avoids holding `&mut g.state` across awaits
            // (which keeps the mutex guard live across yield points).
            let mut state = std::mem::take(&mut g.state);
            drop(g);
            let result = orchestrator::up(&cfg, plugin, &mut state).await;
            let mut g = ctx.lock().await;
            g.state = state;
            match g.persist() {
                Ok(()) => match result {
                    Ok(()) => Response::ok(serde_json::json!({"up": true})),
                    Err(e) => Response::err(format!("up failed: {e:#}")),
                },
                Err(e) => Response::err(format!("persist failed: {e:#}")),
            }
        }
        "down" => {
            let mut g = ctx.lock().await;
            let plugin = g.plugin.clone();
            let cfg = g.config.clone();
            let mut state = std::mem::take(&mut g.state);
            drop(g);
            let result = orchestrator::down(&cfg, plugin, &mut state).await;
            let mut g = ctx.lock().await;
            g.state = state;
            let _ = g.persist();
            match result {
                Ok(()) => Response::ok(serde_json::json!({"down": true})),
                Err(e) => Response::err(format!("down failed: {e:#}")),
            }
        }
        "attach" => {
            let name = match loop_name_from_params(&req.params) {
                Some(s) => s,
                None => return Response::err("attach requires params.name"),
            };
            let g = ctx.lock().await;
            let plugin = g.plugin.clone();
            let cfg = g.config.clone();
            let state = g.state.clone();
            drop(g);
            match orchestrator::attach(&cfg, plugin, &state, &name).await {
                Ok(()) => Response::ok(serde_json::json!({"attached": name})),
                Err(e) => Response::err(format!("attach failed: {e:#}")),
            }
        }
        "delete-loop" => {
            let name = match loop_name_from_params(&req.params) {
                Some(s) => s,
                None => return Response::err("delete-loop requires params.name"),
            };
            let mut g = ctx.lock().await;
            let plugin = g.plugin.clone();
            let mut config = g.config.clone();
            let config_path = g.config_path.clone();
            let mut state = std::mem::take(&mut g.state);
            drop(g);
            let result = crate::orchestrator::delete_loop(
                &mut config,
                &config_path,
                plugin,
                &mut state,
                &name,
            )
            .await;
            let mut g = ctx.lock().await;
            g.state = state;
            g.config = config;
            let _ = g.persist();
            match result {
                Ok(s) => Response::ok(serde_json::json!({
                    "deleted": s.name,
                    "pane_closed": s.pane_closed,
                    "removed_from_config": s.removed_from_config,
                    "readme_block_stripped": s.readme_block_stripped,
                    "state_dropped": s.state_dropped,
                    "dir_removed": s.dir_removed,
                })),
                Err(e) => Response::err(format!("delete-loop failed: {e:#}")),
            }
        }
        "close-loop" => {
            let name = match loop_name_from_params(&req.params) {
                Some(s) => s,
                None => return Response::err("close-loop requires params.name"),
            };
            let mut g = ctx.lock().await;
            let plugin = g.plugin.clone();
            let mut config = g.config.clone();
            let config_path = g.config_path.clone();
            let mut state = std::mem::take(&mut g.state);
            drop(g);
            let result = crate::orchestrator::close_loop(
                &mut config,
                &config_path,
                plugin,
                &mut state,
                &name,
            )
            .await;
            let mut g = ctx.lock().await;
            g.state = state;
            g.config = config;
            let _ = g.persist();
            match result {
                Ok(()) => Response::ok(serde_json::json!({"closed": name})),
                Err(e) => Response::err(format!("close-loop failed: {e:#}")),
            }
        }
        "reopen-loop" => {
            let name = match loop_name_from_params(&req.params) {
                Some(s) => s,
                None => return Response::err("reopen-loop requires params.name"),
            };
            let mut g = ctx.lock().await;
            let plugin = g.plugin.clone();
            let mut config = g.config.clone();
            let config_path = g.config_path.clone();
            let mut state = std::mem::take(&mut g.state);
            drop(g);
            let result = crate::orchestrator::reopen_loop(
                &mut config,
                &config_path,
                plugin,
                &mut state,
                &name,
            )
            .await;
            let mut g = ctx.lock().await;
            g.state = state;
            g.config = config;
            let _ = g.persist();
            match result {
                Ok(()) => Response::ok(serde_json::json!({"reopened": name})),
                Err(e) => Response::err(format!("reopen-loop failed: {e:#}")),
            }
        }
        "update-prompt" => {
            let name = match loop_name_from_params(&req.params) {
                Some(s) => s,
                None => return Response::err("update-prompt requires params.name"),
            };
            let body = match req.params.get("body").and_then(|v| v.as_str()) {
                Some(s) => s.to_string(),
                None => return Response::err("update-prompt requires params.body"),
            };
            let repaste = req
                .params
                .get("repaste")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let g = ctx.lock().await;
            let plugin = g.plugin.clone();
            let config = g.config.clone();
            let state = g.state.clone();
            drop(g);
            let result = crate::orchestrator::update_prompt(
                &config,
                plugin.as_ref(),
                &state,
                &name,
                &body,
                repaste,
            )
            .await;
            // No state mutation; just persist for completeness.
            let mut g = ctx.lock().await;
            let _ = g.persist();
            match result {
                Ok(()) => Response::ok(serde_json::json!({
                    "name": name,
                    "repasted": repaste,
                })),
                Err(e) => Response::err(format!("update-prompt failed: {e:#}")),
            }
        }
        "move-group" => {
            let name = match loop_name_from_params(&req.params) {
                Some(s) => s,
                None => return Response::err("move-group requires params.name"),
            };
            let new_group = match req.params.get("group").and_then(|v| v.as_str()) {
                Some(s) => s.to_string(),
                None => return Response::err("move-group requires params.group"),
            };
            let force = req.params.get("force").and_then(|v| v.as_bool()).unwrap_or(false);

            let mut g = ctx.lock().await;
            let plugin = g.plugin.clone();
            let mut config = g.config.clone();
            let config_path = g.config_path.clone();
            let mut state = std::mem::take(&mut g.state);
            drop(g);

            let result = crate::orchestrator::move_group(
                &mut config,
                &config_path,
                plugin,
                &mut state,
                &name,
                &new_group,
                force,
            )
            .await;

            let mut g = ctx.lock().await;
            g.state = state;
            g.config = config;
            let _ = g.persist();

            match result {
                Ok(crate::orchestrator::MoveGroupOutcome::Moved) => Response::ok(
                    serde_json::json!({"moved": true, "name": name, "group": new_group}),
                ),
                Ok(crate::orchestrator::MoveGroupOutcome::RequiresConfirm {
                    tabs_in_group,
                }) => Response::ok(serde_json::json!({
                    "moved": false,
                    "requires_confirm": true,
                    "name": name,
                    "group": new_group,
                    "tabs_in_group": tabs_in_group,
                })),
                Err(e) => Response::err(format!("move-group failed: {e:#}")),
            }
        }
        "recover" => {
            let mut g = ctx.lock().await;
            let plugin = g.plugin.clone();
            let cfg = g.config.clone();
            let mut state = std::mem::take(&mut g.state);
            drop(g);
            let result =
                crate::orchestrator::health_check_and_recover(&cfg, plugin, &mut state).await;
            let mut g = ctx.lock().await;
            g.state = state;
            let _ = g.persist();
            match result {
                Ok(()) => Response::ok(serde_json::json!({"recovered": true})),
                Err(e) => Response::err(format!("recover failed: {e:#}")),
            }
        }
        "tick" => {
            let name = match loop_name_from_params(&req.params) {
                Some(s) => s,
                None => return Response::err("tick requires params.name"),
            };
            let mut g = ctx.lock().await;
            let plugin = g.plugin.clone();
            let cfg = g.config.clone();
            let mut state = std::mem::take(&mut g.state);
            drop(g);
            let result = dsp::tick_one(&cfg, plugin, &mut state, &name).await;
            let mut g = ctx.lock().await;
            g.state = state;
            let _ = g.persist();
            match result {
                Ok(report) => Response::ok(serde_json::json!({
                    "name": name,
                    "kind": report.kind,
                    "exit": report.exit,
                })),
                Err(e) => Response::err(format!("tick failed: {e:#}")),
            }
        }
        other => Response::err(format!("unknown method: {other}")),
    }
}

/// Compile-time version string emitted by build.rs:
/// `<crate-version>-<git-sha>[-dirty]`. The daemon reports this when
/// the CLI calls the `version` RPC; `loopctl restart` queries before
/// and after the bounce so the user can tell whether an upgrade
/// actually landed.
pub const COMPILED_VERSION: &str = env!("LOOPCTL_VERSION");

fn version_response() -> Response {
    Response::ok(serde_json::json!({ "version": COMPILED_VERSION }))
}

fn build_status(g: &DaemonContext) -> StatusResponse {
    let mut loops = Vec::with_capacity(g.config.loops.len());
    for lc in &g.config.loops {
        let st = g
            .state
            .loops
            .get(&lc.name)
            .cloned()
            .unwrap_or(LoopState::default());
        loops.push(StatusLoop {
            name: lc.name.clone(),
            group_key: lc.group_key.clone(),
            tab_title: lc.effective_tab_title().to_string(),
            schedule: lc.schedule.clone(),
            pane_id: st.pane_id,
            last_tick: st.last_tick,
            next_tick: st.next_tick,
            last_dispatch_kind: st.last_dispatch_kind,
            last_dispatch_exit: st.last_dispatch_exit,
            metrics: st.last_metrics,
            pane_health: st.pane_health,
        });
    }
    StatusResponse {
        terminal: g.state.terminal.clone(),
        loop_count: loops.len(),
        loops,
    }
}

/// Client helper: connect, send one request, read one response.
pub async fn call_once(socket: &Path, req: &Request) -> Result<Response> {
    let stream = UnixStream::connect(socket)
        .await
        .with_context(|| format!("connect {socket:?}"))?;
    let (read, mut write) = stream.into_split();
    let mut json = serde_json::to_string(req)?;
    json.push('\n');
    write.write_all(json.as_bytes()).await?;
    write.flush().await?;
    drop(write);
    let mut reader = BufReader::new(read);
    let mut line = String::new();
    reader.read_line(&mut line).await?;
    let resp: Response = serde_json::from_str(line.trim())?;
    Ok(resp)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn response_serializes_ok_form() {
        let r = Response::ok(serde_json::json!({"x": 1}));
        let s = serde_json::to_string(&r).unwrap();
        assert!(s.contains("\"ok\":true"));
        assert!(s.contains("\"result\""));
    }

    #[test]
    fn response_serializes_err_form() {
        let r = Response::err("nope");
        let s = serde_json::to_string(&r).unwrap();
        assert!(s.contains("\"ok\":false"));
        assert!(s.contains("\"error\":\"nope\""));
    }

    #[test]
    fn request_round_trip() {
        let req = Request {
            method: "tick".into(),
            params: serde_json::json!({"name": "alpha-loop"}),
        };
        let s = serde_json::to_string(&req).unwrap();
        let back: Request = serde_json::from_str(&s).unwrap();
        assert_eq!(back.method, "tick");
        assert_eq!(back.params["name"], "alpha-loop");
    }

    #[test]
    fn version_response_returns_compile_time_string() {
        let resp = version_response();
        match resp {
            Response::Ok { result, .. } => {
                let v = result["version"]
                    .as_str()
                    .expect("version field should be a string");
                assert!(!v.is_empty(), "version must not be empty");
                assert_eq!(v, COMPILED_VERSION);
                // The crate version is always a prefix of the build-time
                // version (with or without git-sha suffix).
                assert!(
                    v.starts_with(env!("CARGO_PKG_VERSION")),
                    "version `{v}` must start with crate version `{}`",
                    env!("CARGO_PKG_VERSION")
                );
            }
            Response::Err { error, .. } => panic!("unexpected error: {error}"),
        }
    }

    #[test]
    fn loop_name_from_params_prefers_name_then_slug() {
        assert_eq!(
            loop_name_from_params(&serde_json::json!({"name": "x"})),
            Some("x".to_string())
        );
        assert_eq!(
            loop_name_from_params(&serde_json::json!({"slug": "y"})),
            Some("y".to_string())
        );
        // Both present: name wins.
        assert_eq!(
            loop_name_from_params(&serde_json::json!({"name": "n", "slug": "s"})),
            Some("n".to_string())
        );
        assert_eq!(
            loop_name_from_params(&serde_json::json!({"other": "z"})),
            None
        );
    }
}