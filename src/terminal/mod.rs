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

//! Terminal backend abstraction.
//!
//! The daemon and CLI never talk to Konsole/tmux/Terminal.app directly —
//! they go through [`TerminalPlugin`]. v1 ships the Konsole plugin; tmux
//! and macOS plugins are stubs that document the intended surface.

use std::path::{Path, PathBuf};

use async_trait::async_trait;

pub mod iterm2;
pub mod konsole;
pub mod mac;
pub mod mock;
pub mod tmux;

/// Opaque, plugin-defined identifier for a single pane/tab.
///
/// Konsole format: `konsole:<bus-pid>:<session-id>`.
/// Future tmux: `tmux:<session>:<window-index>`.
/// Future Terminal.app: `mac:<window-id>:<tab-index>`.
///
/// The daemon persists this verbatim in `state.json` so it can re-attach
/// after a restart without re-spawning panes.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PaneId(pub String);

impl PaneId {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Spec for ensuring a pane exists in a group.
///
/// Backends MUST be idempotent on `(group_key, title)` — if a matching pane
/// is already present the existing `PaneId` should be returned without
/// running `initial_command` a second time.
#[derive(Debug, Clone)]
pub struct PaneSpec {
    /// Backend-specific group identifier — Konsole desktop number,
    /// tmux session name, Terminal.app window id, etc.
    pub group_key: String,
    /// Human-readable tab/pane title. Stable identity across runs.
    pub title: String,
    /// Working directory for the pane's shell.
    pub workdir: PathBuf,
    /// Command to run after the shell starts. `None` ⇒ leave at the prompt.
    /// On Konsole this is `runCommand`; on tmux it's `send-keys`.
    pub initial_command: Option<String>,
}

/// Reasons a backend can refuse to run on the current host.
#[derive(Debug, thiserror::Error)]
pub enum UnsupportedReason {
    #[error("backend `{backend}` is not supported on this platform: {detail}")]
    WrongPlatform { backend: &'static str, detail: String },
    #[error("backend `{backend}` requires {requirement} which was not found")]
    MissingDependency { backend: &'static str, requirement: String },
    #[error("backend `{backend}` is not yet implemented")]
    NotImplemented { backend: &'static str },
}

/// Result alias for plugin operations.
pub type PluginResult<T> = Result<T, anyhow::Error>;

/// Contract every terminal backend implements.
///
/// All ops MUST be idempotent. Calling `ensure_group` or `ensure_pane`
/// twice with the same arguments must not produce duplicate windows or
/// tabs — the second call returns the existing handle.
#[async_trait]
pub trait TerminalPlugin: Send + Sync {
    /// Stable backend identifier — used in config (`workbench.terminal`)
    /// and in pane id prefixes.
    fn name(&self) -> &'static str;

    /// Pre-flight: refuse early on the wrong platform / session type.
    /// Called once at daemon startup.
    fn supports_environment(&self) -> Result<(), UnsupportedReason>;

    /// Ensure a "group" container exists (Konsole window per desktop, tmux
    /// session, Terminal.app window). Idempotent.
    async fn ensure_group(&self, group_key: &str) -> PluginResult<()>;

    /// Ensure a pane matching `spec.title` exists in the group, returning
    /// its [`PaneId`]. Idempotent on (group_key, title). If the pane is
    /// newly created and `spec.initial_command` is set, run it inside.
    async fn ensure_pane(&self, spec: &PaneSpec) -> PluginResult<PaneId>;

    /// Paste raw text into a pane. No shell interpretation. Used for resume
    /// prompts. Newlines normalized internally.
    async fn paste(&self, pane: &PaneId, text: &str) -> PluginResult<()>;

    /// Run a command in a pane. Treats `command` as a shell line.
    async fn run(&self, pane: &PaneId, command: &str) -> PluginResult<()>;

    /// Bring a pane to the foreground (focus + raise).
    async fn focus(&self, pane: &PaneId) -> PluginResult<()>;

    /// Close a pane.
    async fn close(&self, pane: &PaneId) -> PluginResult<()>;

    /// Health probe: is this pane still real and reachable?
    /// Returning Ok(false) signals "the user (or a crash) closed it";
    /// the daemon will mark the loop crashed and attempt to recreate.
    /// Network/transient errors should bubble up as Err so the daemon
    /// can leave health untouched rather than acting on a flake.
    async fn pane_alive(&self, pane: &PaneId) -> PluginResult<bool>;

    /// Snapshot the in-process group cache for persistence into
    /// state.json. Default: empty (plugins without group containers
    /// don't need this). Konsole overrides.
    fn snapshot_groups(
        &self,
    ) -> std::collections::BTreeMap<String, crate::state::GroupState> {
        std::collections::BTreeMap::new()
    }

    /// Re-adopt previously-spawned containers from a persisted state.
    /// Default: no-op. Konsole overrides — it walks running konsoles
    /// looking for ones whose first session env contains a stored uuid.
    async fn rehydrate(
        &self,
        _previous: &std::collections::BTreeMap<String, crate::state::GroupState>,
    ) -> PluginResult<()> {
        Ok(())
    }

    /// Look up an existing pane in `group_key` by its tab title. Used at
    /// daemon startup to re-link `state.loops[name].pane_id` after a
    /// rehydrate (the group's konsole survives, but state.json doesn't
    /// remember which tab inside it belonged to which loop). Default
    /// returns None so plugins without group-with-tabs semantics no-op.
    async fn find_pane_by_title(
        &self,
        _group_key: &str,
        _title: &str,
    ) -> PluginResult<Option<PaneId>> {
        Ok(None)
    }

    /// Drop any cached handle for `group_key`. Used during crash
    /// recovery: when a managed konsole has died we need the next
    /// `ensure_group` call to spawn a fresh one rather than reuse a
    /// stale pid. Default: no-op for plugins without a group cache.
    fn forget_group(&self, _group_key: &str) {}

    /// Move the entire group container (Konsole window) from one
    /// `group_key` to another. For Konsole, the `group_key` is the X11
    /// virtual desktop number, so this calls KWin to relocate. Default:
    /// returns NotImplemented; Konsole overrides.
    async fn move_group_to(&self, _old_key: &str, _new_key: &str) -> PluginResult<()> {
        Err(anyhow::anyhow!(
            "move_group_to is not implemented for this plugin"
        ))
    }

    /// Rename the in-memory cache key for a group (after a successful
    /// `move_group_to`). Default: no-op.
    fn rekey_group(&self, _old_key: &str, _new_key: &str) {}

    /// Return the list of desktop numbers (1-indexed) that currently have
    /// zero non-sticky windows. Used by the repo loader to pack new loops
    /// into empty desktops first. `desktop_count` is the total number of
    /// virtual desktops to consider.
    ///
    /// Default: returns an empty list (plugins without a notion of
    /// virtual desktops can't answer).
    async fn empty_desktops(&self, _desktop_count: u32) -> PluginResult<Vec<u32>> {
        Ok(Vec::new())
    }

    /// Total number of virtual desktops the host advertises. Default 0
    /// for plugins without virtual desktops (the repo-loader treats 0
    /// as "I don't know" and falls back to a fixed [7..=9] range).
    async fn desktop_count(&self) -> PluginResult<u32> {
        Ok(0)
    }
}

/// Normalize line endings before paste. Claude is sensitive to bare CR.
pub fn normalize_paste(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

/// Append a trailing newline if missing — needed so `sendText` actually
/// submits the prompt rather than leaving it queued.
pub fn ensure_trailing_newline(s: &mut String) {
    if !s.ends_with('\n') {
        s.push('\n');
    }
}

/// Helper for test/diagnostics callers that want a quick `PaneSpec`.
pub fn pane_spec(
    group_key: impl Into<String>,
    title: impl Into<String>,
    workdir: impl AsRef<Path>,
    initial_command: Option<impl Into<String>>,
) -> PaneSpec {
    PaneSpec {
        group_key: group_key.into(),
        title: title.into(),
        workdir: workdir.as_ref().to_path_buf(),
        initial_command: initial_command.map(Into::into),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_paste_handles_crlf_and_bare_cr() {
        assert_eq!(normalize_paste("a\r\nb"), "a\nb");
        assert_eq!(normalize_paste("a\rb"), "a\nb");
        assert_eq!(normalize_paste("plain\n"), "plain\n");
    }

    #[test]
    fn ensure_trailing_newline_idempotent() {
        let mut s = String::from("a\n");
        ensure_trailing_newline(&mut s);
        assert_eq!(s, "a\n");
        let mut s = String::from("a");
        ensure_trailing_newline(&mut s);
        assert_eq!(s, "a\n");
    }

    #[test]
    fn pane_id_round_trip() {
        let id = PaneId::new("konsole:1234:7");
        assert_eq!(id.as_str(), "konsole:1234:7");
    }
}