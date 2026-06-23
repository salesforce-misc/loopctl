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

//! macOS Terminal.app / iTerm2 backend — STUB.
//!
//! Sketch for future implementation:
//!
//! Terminal.app:
//! - `group_key` = Terminal.app window id (string).
//! - `ensure_group(key)` runs:
//!   `osascript -e 'tell application "Terminal" to do script ""'`
//!   if no recorded window id, then captures the new window's id.
//! - `ensure_pane(spec)` opens a new tab in that window with AppleScript:
//!   `tell application "Terminal" to set newTab to do script "cd <dir>" in window <id>`
//!   Pane id: `mac:<window-id>:<tab-id>`.
//! - `paste(pane, text)` simulates keystrokes via System Events — there's
//!   no clean `sendText`. This is the main pain point — long pastes are
//!   slow and intercepted by anything with global hotkeys.
//! - `run(pane, cmd)` is `do script <cmd> in <pane>`.
//! - `focus(pane)` brings the tab to the front.
//! - `close(pane)` closes the tab.
//!
//! iTerm2 has a richer Python API and would likely be a separate plugin
//! (`iterm2`) using the iTerm2 WebSocket protocol — much cleaner paste.
//!
//! supports_environment will require macOS at runtime
//! (`std::env::consts::OS == "macos"`) plus availability of `osascript`.

use async_trait::async_trait;

use super::{PaneId, PaneSpec, PluginResult, TerminalPlugin, UnsupportedReason};

pub struct MacTerminalPlugin;

#[async_trait]
impl TerminalPlugin for MacTerminalPlugin {
    fn name(&self) -> &'static str {
        "mac-terminal"
    }
    fn supports_environment(&self) -> Result<(), UnsupportedReason> {
        Err(UnsupportedReason::NotImplemented {
            backend: self.name(),
        })
    }
    async fn ensure_group(&self, _key: &str) -> PluginResult<()> {
        unimplemented!("macOS Terminal backend not yet implemented")
    }
    async fn ensure_pane(&self, _spec: &PaneSpec) -> PluginResult<PaneId> {
        unimplemented!("macOS Terminal backend not yet implemented")
    }
    async fn paste(&self, _pane: &PaneId, _text: &str) -> PluginResult<()> {
        unimplemented!("macOS Terminal backend not yet implemented")
    }
    async fn run(&self, _pane: &PaneId, _command: &str) -> PluginResult<()> {
        unimplemented!("macOS Terminal backend not yet implemented")
    }
    async fn focus(&self, _pane: &PaneId) -> PluginResult<()> {
        unimplemented!("macOS Terminal backend not yet implemented")
    }
    async fn close(&self, _pane: &PaneId) -> PluginResult<()> {
        unimplemented!("macOS Terminal backend not yet implemented")
    }
    async fn pane_alive(&self, _pane: &PaneId) -> PluginResult<bool> {
        unimplemented!("macOS Terminal backend not yet implemented")
    }
}