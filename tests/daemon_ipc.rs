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

//! Daemon IPC integration tests. Spin up the daemon in-process with the
//! mock plugin, drive RPC over a real Unix socket in a tempdir, and assert
//! the JSON payloads round-trip correctly.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use agent_loop_workbench::config::WorkbenchConfig;
use agent_loop_workbench::daemon::{ipc, run};
use agent_loop_workbench::terminal::mock::MockTerminalPlugin;

const MIN_CONFIG: &str = r#"
schema_version = 1

[workbench]
claude_loops_dir = "~/claude-loops"
terminal = "konsole"
central_group = "1"

[[loops]]
name = "alpha-loop"
dir = "alpha-loop"
readme_anchor = "alpha-loop--foo-bar-perf-tuning"
group_key = "2"
schedule = "every 10m"
dispatch = "tick_script"
tick_script = "skill/scripts/tick.py"

[[loops]]
name = "beta-loop"
dir = "beta-loop"
readme_anchor = "beta-loop--issue-tracker"
group_key = "2"
schedule = "every 15m"
dispatch = "tick_script"
tick_script = "skill/scripts/tick.py"
"#;

struct Harness {
    socket: PathBuf,
    state: PathBuf,
    daemon: tokio::task::JoinHandle<anyhow::Result<()>>,
}

impl Harness {
    async fn start() -> Self {
        let dir = tempfile::tempdir().unwrap().keep();
        let socket = dir.join("loopctl.sock");
        let state = dir.join("state.json");
        let cfg_path = dir.join("workbench.toml");
        std::fs::write(&cfg_path, MIN_CONFIG).unwrap();
        let cfg: WorkbenchConfig =
            agent_loop_workbench::config::parse(MIN_CONFIG).expect("parse cfg");
        let plugin = Arc::new(MockTerminalPlugin::new());
        let socket_for_daemon = socket.clone();
        let state_for_daemon = state.clone();
        let daemon = tokio::spawn(async move {
            run(cfg, cfg_path, plugin, state_for_daemon, socket_for_daemon).await
        });
        // Poll for the socket to appear so the test isn't racy.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        while !socket.exists() {
            if tokio::time::Instant::now() >= deadline {
                panic!("daemon never created socket");
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        Self {
            socket,
            state,
            daemon,
        }
    }

    async fn rpc(&self, method: &str) -> ipc::Response {
        let req = ipc::Request {
            method: method.into(),
            params: serde_json::Value::Null,
        };
        ipc::call_once(&self.socket, &req)
            .await
            .expect("rpc call_once")
    }

    async fn shutdown(self) {
        let _ = self.rpc("shutdown").await;
        // Give the daemon a moment to flush state and exit.
        let _ = tokio::time::timeout(Duration::from_secs(2), self.daemon).await;
    }
}

#[tokio::test(flavor = "current_thread")]
async fn ping_pong() {
    let h = Harness::start().await;
    let resp = h.rpc("ping").await;
    match resp {
        ipc::Response::Ok { result, .. } => {
            assert_eq!(result["pong"], true);
        }
        ipc::Response::Err { error, .. } => panic!("ping returned error: {error}"),
    }
    h.shutdown().await;
}

#[tokio::test(flavor = "current_thread")]
async fn status_lists_configured_loops_with_defaults() {
    let h = Harness::start().await;
    let resp = h.rpc("status").await;
    let value = match resp {
        ipc::Response::Ok { result, .. } => result,
        ipc::Response::Err { error, .. } => panic!("status returned error: {error}"),
    };
    let s: ipc::StatusResponse = serde_json::from_value(value).unwrap();
    assert_eq!(s.terminal, "mock");
    assert_eq!(s.loop_count, 2);
    let names: Vec<_> = s.loops.iter().map(|l| l.name.as_str()).collect();
    assert!(names.contains(&"alpha-loop"));
    assert!(names.contains(&"beta-loop"));
    let cc = s.loops.iter().find(|l| l.name == "alpha-loop").unwrap();
    assert_eq!(cc.tab_title, "alpha-loop"); // defaulted from name
    assert!(cc.pane_id.is_none()); // up not yet implemented
    assert!(cc.last_tick.is_none());
    h.shutdown().await;
}

#[tokio::test(flavor = "current_thread")]
async fn unknown_method_returns_error() {
    let h = Harness::start().await;
    let resp = h.rpc("not-a-real-method").await;
    match resp {
        ipc::Response::Err { error, .. } => assert!(error.contains("unknown method")),
        ipc::Response::Ok { .. } => panic!("expected error"),
    }
    h.shutdown().await;
}

#[tokio::test(flavor = "current_thread")]
async fn shutdown_persists_state_to_disk() {
    let h = Harness::start().await;
    let state_path = h.state.clone();
    h.shutdown().await;
    let body = std::fs::read_to_string(&state_path).expect("state file persisted");
    assert!(body.contains("\"terminal\": \"mock\""));
}

#[tokio::test(flavor = "current_thread")]
async fn tick_requires_name_param() {
    let h = Harness::start().await;
    let req = ipc::Request {
        method: "tick".into(),
        params: serde_json::Value::Null,
    };
    let resp = ipc::call_once(&h.socket, &req).await.unwrap();
    match resp {
        ipc::Response::Err { error, .. } => assert!(error.contains("name")),
        ipc::Response::Ok { .. } => panic!("expected error"),
    }
    h.shutdown().await;
}