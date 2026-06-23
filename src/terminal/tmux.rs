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

//! tmux backend — STUB.
//!
//! Sketch for future implementation:
//!
//! - `group_key` = tmux session name.
//! - `ensure_group(key)` runs `tmux new-session -d -s <key>` (no-op if exists).
//! - `ensure_pane(spec)` runs:
//!   `tmux new-window -t <key> -n <title> -c <cwd>`
//!   then captures the resulting window index from `tmux list-windows`.
//!   Pane id format: `tmux:<key>:<window-index>`.
//! - `paste(pane, text)` writes a buffer and pastes it:
//!   `tmux load-buffer -` (stdin) + `paste-buffer -t <pane>`.
//! - `run(pane, cmd)` is `tmux send-keys -t <pane> <cmd> Enter`.
//! - `focus(pane)` is `tmux select-window -t <pane>`. There's no equivalent
//!   of "raising" outside of the user's current attached client.
//! - `close(pane)` is `tmux kill-window -t <pane>`.
//!
//! Best for: headless / SSH / remote dev box workflows. macOS dev users
//! who like terminal multiplexing also benefit.

use async_trait::async_trait;

use super::{PaneId, PaneSpec, PluginResult, TerminalPlugin, UnsupportedReason};

pub struct TmuxPlugin;

#[async_trait]
impl TerminalPlugin for TmuxPlugin {
    fn name(&self) -> &'static str {
        "tmux"
    }
    fn supports_environment(&self) -> Result<(), UnsupportedReason> {
        Err(UnsupportedReason::NotImplemented {
            backend: self.name(),
        })
    }
    async fn ensure_group(&self, _key: &str) -> PluginResult<()> {
        unimplemented!("tmux backend not yet implemented")
    }
    async fn ensure_pane(&self, _spec: &PaneSpec) -> PluginResult<PaneId> {
        unimplemented!("tmux backend not yet implemented")
    }
    async fn paste(&self, _pane: &PaneId, _text: &str) -> PluginResult<()> {
        unimplemented!("tmux backend not yet implemented")
    }
    async fn run(&self, _pane: &PaneId, _command: &str) -> PluginResult<()> {
        unimplemented!("tmux backend not yet implemented")
    }
    async fn focus(&self, _pane: &PaneId) -> PluginResult<()> {
        unimplemented!("tmux backend not yet implemented")
    }
    async fn close(&self, _pane: &PaneId) -> PluginResult<()> {
        unimplemented!("tmux backend not yet implemented")
    }
    async fn pane_alive(&self, _pane: &PaneId) -> PluginResult<bool> {
        unimplemented!("tmux backend not yet implemented")
    }
}