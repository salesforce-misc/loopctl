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

//! Persistent state model. Serialized as JSON; the on-disk shape is
//! versioned by `schema_version`.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Top-level state. Writes are atomic; see [`crate::state::save`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GlobalState {
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
    /// Plugin name in use last time the daemon ran. Helps surface
    /// "you switched terminals" warnings on startup.
    #[serde(default)]
    pub terminal: String,
    /// When the daemon most recently started.
    #[serde(default)]
    pub started_at: Option<DateTime<Utc>>,
    /// Per-group container handles. Keyed by `group_key`.
    #[serde(default)]
    pub groups: BTreeMap<String, GroupState>,
    /// Per-loop state. Keyed by loop name.
    #[serde(default)]
    pub loops: BTreeMap<String, LoopState>,
}

fn default_schema_version() -> u32 {
    1
}

impl GlobalState {
    pub fn new(terminal: impl Into<String>) -> Self {
        Self {
            schema_version: 1,
            terminal: terminal.into(),
            started_at: Some(Utc::now()),
            groups: BTreeMap::new(),
            loops: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupState {
    pub pid: i32,
    pub bus_name: String,
    /// Plugin-defined re-adoption tag. For the Konsole plugin: a short
    /// uuid written into the konsole's first-session env as
    /// `LOOPCTL_GROUP_UUID=<uuid>`. Lets the daemon re-attach across
    /// restarts without spawning duplicates.
    #[serde(default)]
    pub uuid: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LoopState {
    /// Pane id from the active TerminalPlugin. None ⇒ pane never created.
    #[serde(default)]
    pub pane_id: Option<String>,
    #[serde(default)]
    pub last_tick: Option<DateTime<Utc>>,
    #[serde(default)]
    pub next_tick: Option<DateTime<Utc>>,
    /// Free-form: "resume_prompt" | "tick_script" | "none".
    #[serde(default)]
    pub last_dispatch_kind: Option<String>,
    #[serde(default)]
    pub last_dispatch_exit: Option<i32>,
    #[serde(default)]
    pub last_metrics: Metrics,
    /// Lifecycle of the pane behind `pane_id`. The daemon's health-check
    /// pass updates this every minute.
    #[serde(default)]
    pub pane_health: PaneHealth,
}

/// Lifecycle of a managed pane.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum PaneHealth {
    /// Never been spawned, or spawned and never health-checked.
    #[default]
    Unknown,
    /// Health check confirmed the pane is alive.
    Running,
    /// Health check detected the pane is gone. Daemon will attempt to
    /// recreate on the next health pass.
    Crashed { since: DateTime<Utc> },
    /// Recreation attempts have failed. Continues to retry every health
    /// pass; surfaces last error for the TUI.
    FailedRecreate {
        since: DateTime<Utc>,
        attempts: u32,
        last_error: String,
    },
    /// Loop is `active = false` in workbench.toml. Daemon doesn't
    /// spawn, tick, or recover panes for this loop. Reopen it via
    /// `loopctl reopen <name>`.
    Inactive { since: DateTime<Utc> },
}

impl PaneHealth {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Running => "running",
            Self::Crashed { .. } => "crashed",
            Self::FailedRecreate { .. } => "failed-recreate",
            Self::Inactive { .. } => "inactive",
        }
    }

    pub fn needs_recreate(&self) -> bool {
        matches!(self, Self::Crashed { .. } | Self::FailedRecreate { .. })
    }
}

/// Heuristic-extracted metrics. Stored in two open buckets so the state
/// schema doesn't need a migration every time a new extractor lands.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Metrics {
    #[serde(default)]
    pub integers: BTreeMap<String, i64>,
    #[serde(default)]
    pub strings: BTreeMap<String, String>,
    /// Last-modified mtime for the file the extractors read from.
    #[serde(default)]
    pub mtimes: BTreeMap<String, DateTime<Utc>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_empty_and_safe() {
        let s = GlobalState::default();
        assert_eq!(s.schema_version, 0); // default() leaves it 0; new() sets 1
        assert!(s.groups.is_empty());
        assert!(s.loops.is_empty());
    }

    #[test]
    fn json_round_trip_preserves_btreemap_order() {
        let mut s = GlobalState::new("konsole");
        s.loops.insert("b".into(), LoopState::default());
        s.loops.insert("a".into(), LoopState::default());
        let json = serde_json::to_string_pretty(&s).unwrap();
        // BTreeMap keys are sorted, so "a" must come before "b" in json output
        let pos_a = json.find("\"a\"").unwrap();
        let pos_b = json.find("\"b\"").unwrap();
        assert!(pos_a < pos_b);
    }

    #[test]
    fn loop_state_round_trip_with_optional_fields() {
        let s = LoopState {
            pane_id: Some("konsole:1:2".into()),
            last_tick: Some(Utc::now()),
            next_tick: None,
            last_dispatch_kind: Some("tick_script".into()),
            last_dispatch_exit: Some(0),
            last_metrics: Metrics::default(),
            pane_health: PaneHealth::Running,
        };
        let json = serde_json::to_string(&s).unwrap();
        let back: LoopState = serde_json::from_str(&json).unwrap();
        assert_eq!(back.pane_id, s.pane_id);
        assert_eq!(back.last_dispatch_exit, s.last_dispatch_exit);
    }
}