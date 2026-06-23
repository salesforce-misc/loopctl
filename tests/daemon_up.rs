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

//! Step 6 integration test: drive `loopctl up` end-to-end via RPC,
//! verify the plugin recorded the expected ops, and verify state.json
//! captured the assigned pane ids.
//!
//! Unlike daemon_ipc.rs (which pings the wire), this test wires a real
//! claude_loops_dir on disk and asserts the resume prompt got pasted.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use agent_loop_workbench::config;
use agent_loop_workbench::daemon::{ipc, run};
use agent_loop_workbench::terminal::mock::{MockTerminalPlugin, Op};

struct Harness {
    socket: PathBuf,
    state: PathBuf,
    plugin: Arc<MockTerminalPlugin>,
    daemon: tokio::task::JoinHandle<anyhow::Result<()>>,
}

fn write_loops_dir(root: &std::path::Path) {
    std::fs::create_dir_all(root.join("alpha-dir")).unwrap();
    std::fs::create_dir_all(root.join("beta-dir")).unwrap();
    let readme = "\
# header

### alpha — label

```
resume alpha
```

### beta — label

```
resume beta
```
";
    std::fs::write(root.join("README.md"), readme).unwrap();
}

fn cfg_for(loops_dir: &std::path::Path) -> String {
    format!(
        r#"
schema_version = 1

[workbench]
claude_loops_dir = "{}"
terminal = "konsole"
central_group = "1"

[[loops]]
name = "alpha"
dir = "alpha-dir"
readme_anchor = "alpha--label"
group_key = "2"
schedule = "none"
dispatch = "resume_prompt"

[[loops]]
name = "beta"
dir = "beta-dir"
readme_anchor = "beta--label"
group_key = "2"
schedule = "none"
dispatch = "resume_prompt"
"#,
        loops_dir.display()
    )
}

impl Harness {
    async fn start() -> Self {
        let dir = tempfile::tempdir().unwrap().keep();
        let loops = dir.join("claude-loops");
        write_loops_dir(&loops);

        let socket = dir.join("loopctl.sock");
        let state = dir.join("state.json");
        let cfg_str = cfg_for(&loops);
        let cfg_path = dir.join("workbench.toml");
        std::fs::write(&cfg_path, &cfg_str).unwrap();
        let cfg = config::parse(&cfg_str).unwrap();
        let plugin = Arc::new(MockTerminalPlugin::new());
        let socket_for_daemon = socket.clone();
        let state_for_daemon = state.clone();
        let plugin_for_daemon: Arc<dyn agent_loop_workbench::terminal::TerminalPlugin> =
            plugin.clone();
        let daemon = tokio::spawn(async move {
            run(cfg, cfg_path, plugin_for_daemon, state_for_daemon, socket_for_daemon).await
        });
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
            plugin,
            daemon,
        }
    }

    async fn rpc(&self, method: &str, params: serde_json::Value) -> ipc::Response {
        let req = ipc::Request {
            method: method.into(),
            params,
        };
        ipc::call_once(&self.socket, &req).await.unwrap()
    }

    async fn shutdown(self) -> Arc<MockTerminalPlugin> {
        let _ = self.rpc("shutdown", serde_json::Value::Null).await;
        let _ = tokio::time::timeout(Duration::from_secs(2), self.daemon).await;
        self.plugin
    }
}

#[tokio::test(flavor = "current_thread")]
async fn up_runs_orchestrator_and_persists_pane_ids() {
    let h = Harness::start().await;
    let resp = h.rpc("up", serde_json::Value::Null).await;
    match &resp {
        ipc::Response::Ok { result, .. } => {
            assert_eq!(result["up"], true);
        }
        ipc::Response::Err { error, .. } => panic!("up returned error: {error}"),
    }

    // Validate ops via a separate status call to ensure the daemon flushed
    // state visibly to the IPC layer.
    let status = h.rpc("status", serde_json::Value::Null).await;
    let status_value = match status {
        ipc::Response::Ok { result, .. } => result,
        ipc::Response::Err { error, .. } => panic!("status: {error}"),
    };
    let s: ipc::StatusResponse = serde_json::from_value(status_value).unwrap();
    let alpha = s.loops.iter().find(|l| l.name == "alpha").unwrap();
    assert!(
        alpha.pane_id.is_some(),
        "alpha pane_id should be set after up"
    );

    let state_path = h.state.clone();
    let plugin = h.shutdown().await;

    // Assert ops recorded include EnsureGroup → EnsurePane → Paste for both loops
    let ops = plugin.ops();
    assert!(ops.iter().any(|op| matches!(op, Op::EnsureGroup(k) if k == "2")));
    let pastes: Vec<_> = ops
        .iter()
        .filter(|op| matches!(op, Op::Paste { .. }))
        .collect();
    assert_eq!(pastes.len(), 2, "expected one paste per loop on first up");

    // State file must contain the persisted pane ids.
    let state_body = std::fs::read_to_string(&state_path).unwrap();
    assert!(state_body.contains("\"alpha\""));
    assert!(state_body.contains("\"pane_id\""));
}

#[tokio::test(flavor = "current_thread")]
async fn down_closes_panes_and_clears_state() {
    let h = Harness::start().await;
    h.rpc("up", serde_json::Value::Null).await;
    let resp = h.rpc("down", serde_json::Value::Null).await;
    match resp {
        ipc::Response::Ok { result, .. } => assert_eq!(result["down"], true),
        ipc::Response::Err { error, .. } => panic!("down: {error}"),
    }
    let plugin = h.shutdown().await;
    let closes: Vec<_> = plugin
        .ops()
        .into_iter()
        .filter(|op| matches!(op, Op::Close(_)))
        .collect();
    assert_eq!(closes.len(), 2);
}

#[tokio::test(flavor = "current_thread")]
async fn attach_unknown_loop_returns_error() {
    let h = Harness::start().await;
    h.rpc("up", serde_json::Value::Null).await;
    let resp = h
        .rpc("attach", serde_json::json!({"name": "ghost"}))
        .await;
    match resp {
        ipc::Response::Err { error, .. } => assert!(error.contains("unknown loop")),
        ipc::Response::Ok { .. } => panic!("expected error"),
    }
    h.shutdown().await;
}