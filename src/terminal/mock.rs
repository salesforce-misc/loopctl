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

//! In-memory `TerminalPlugin` for tests. Records every call so the daemon
//! integration tests (step 5) can assert "ensure_group fired before
//! ensure_pane" and "paste was called with the resume prompt".

use std::sync::Mutex;

use async_trait::async_trait;

use super::{PaneId, PaneSpec, PluginResult, TerminalPlugin, UnsupportedReason};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Op {
    EnsureGroup(String),
    EnsurePane {
        group: String,
        title: String,
        workdir: String,
        initial_command: Option<String>,
    },
    Paste {
        pane: String,
        text: String,
    },
    Run {
        pane: String,
        command: String,
    },
    Focus(String),
    Close(String),
}

#[derive(Default)]
pub struct MockTerminalPlugin {
    ops: Mutex<Vec<Op>>,
    panes: Mutex<Vec<(String, String, PaneId)>>, // (group, title, id)
    next_pane_id: Mutex<u32>,
    /// Pane ids that should report dead on the next pane_alive() probe.
    /// Tests fill this to simulate user-closed konsoles.
    dead_panes: Mutex<std::collections::HashSet<String>>,
}

impl MockTerminalPlugin {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn ops(&self) -> Vec<Op> {
        self.ops.lock().unwrap().clone()
    }
    fn record(&self, op: Op) {
        self.ops.lock().unwrap().push(op);
    }
    fn next_id(&self) -> PaneId {
        let mut n = self.next_pane_id.lock().unwrap();
        *n += 1;
        PaneId::new(format!("mock:{n}"))
    }
    /// Mark a pane id as dead so the next `pane_alive` returns false.
    pub fn kill(&self, id: &PaneId) {
        self.dead_panes.lock().unwrap().insert(id.0.clone());
        // Also drop it from the registry so a fresh ensure_pane gets a
        // new id rather than reviving the dead one.
        self.panes.lock().unwrap().retain(|(_, _, p)| p != id);
    }
}

#[async_trait]
impl TerminalPlugin for MockTerminalPlugin {
    fn name(&self) -> &'static str {
        "mock"
    }
    fn supports_environment(&self) -> Result<(), UnsupportedReason> {
        Ok(())
    }
    async fn ensure_group(&self, key: &str) -> PluginResult<()> {
        self.record(Op::EnsureGroup(key.to_string()));
        Ok(())
    }
    async fn ensure_pane(&self, spec: &PaneSpec) -> PluginResult<PaneId> {
        self.record(Op::EnsurePane {
            group: spec.group_key.clone(),
            title: spec.title.clone(),
            workdir: spec.workdir.to_string_lossy().to_string(),
            initial_command: spec.initial_command.clone(),
        });
        // Idempotent on (group, title).
        let panes = self.panes.lock().unwrap();
        if let Some((_, _, id)) = panes
            .iter()
            .find(|(g, t, _)| g == &spec.group_key && t == &spec.title)
        {
            return Ok(id.clone());
        }
        drop(panes);
        let id = self.next_id();
        self.panes
            .lock()
            .unwrap()
            .push((spec.group_key.clone(), spec.title.clone(), id.clone()));
        Ok(id)
    }
    async fn paste(&self, pane: &PaneId, text: &str) -> PluginResult<()> {
        self.record(Op::Paste {
            pane: pane.0.clone(),
            text: text.to_string(),
        });
        Ok(())
    }
    async fn run(&self, pane: &PaneId, command: &str) -> PluginResult<()> {
        self.record(Op::Run {
            pane: pane.0.clone(),
            command: command.to_string(),
        });
        Ok(())
    }
    async fn focus(&self, pane: &PaneId) -> PluginResult<()> {
        self.record(Op::Focus(pane.0.clone()));
        Ok(())
    }
    async fn close(&self, pane: &PaneId) -> PluginResult<()> {
        self.record(Op::Close(pane.0.clone()));
        Ok(())
    }
    async fn pane_alive(&self, pane: &PaneId) -> PluginResult<bool> {
        Ok(!self.dead_panes.lock().unwrap().contains(&pane.0))
    }
}