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

//! iTerm2 backend for macOS.
//!
//! Uses AppleScript (`osascript`) to drive iTerm2. Each "group" maps to an
//! iTerm2 window; each "pane" is a tab within that window. Tabs are matched
//! by their session name (title), which provides idempotency.
//!
//! PaneId format: `iterm2:<window_id>:<session_unique_id>`
//! (window_id is numeric; session_unique_id is a UUID string)

use std::collections::HashMap;
use std::time::Duration;

use anyhow::{anyhow, bail, Context};
use async_trait::async_trait;
use parking_lot::RwLock;
use tokio::process::Command;
use tokio::time::sleep;

use super::{normalize_paste, PaneId, PaneSpec, PluginResult, TerminalPlugin, UnsupportedReason};

fn make_pane_id(window_id: &str, session_id: &str) -> PaneId {
    PaneId::new(format!("iterm2:{window_id}:{session_id}"))
}

fn parse_pane_id(id: &PaneId) -> anyhow::Result<(String, String)> {
    let s = id.as_str();
    let rest = s
        .strip_prefix("iterm2:")
        .ok_or_else(|| anyhow!("not an iterm2 pane id: {s}"))?;
    let (window_id, session_id) = rest
        .split_once(':')
        .ok_or_else(|| anyhow!("malformed iterm2 pane id: {s}"))?;
    Ok((window_id.to_string(), session_id.to_string()))
}

#[derive(Debug, Default)]
struct Cache {
    /// group_key → window_id (numeric string)
    groups: HashMap<String, String>,
}

pub struct Iterm2Plugin {
    cache: RwLock<Cache>,
    paste_submit_delay_ms: u64,
}

impl Default for Iterm2Plugin {
    fn default() -> Self {
        Self::new()
    }
}

impl Iterm2Plugin {
    pub fn new() -> Self {
        Self {
            cache: RwLock::new(Cache::default()),
            paste_submit_delay_ms: 700,
        }
    }

    pub fn with_paste_submit_delay_ms(mut self, ms: u64) -> Self {
        self.paste_submit_delay_ms = ms;
        self
    }

    async fn run_applescript(script: &str) -> PluginResult<String> {
        let tmp = std::env::temp_dir().join(format!(
            "loopctl-iterm2-{}.scpt",
            std::process::id()
        ));
        std::fs::write(&tmp, script).context("write applescript temp file")?;
        let output = Command::new("osascript")
            .arg(&tmp)
            .output()
            .await
            .context("run osascript from file")?;
        let _ = std::fs::remove_file(&tmp);
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("osascript failed: {stderr}");
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    /// Find the window ID for a window that has a session named with our
    /// group marker prefix.
    async fn find_window_by_group(group_key: &str) -> PluginResult<Option<String>> {
        let marker = format!("loopctl-{group_key}");
        let escaped_marker = marker.replace('"', "\\\"");
        let script = format!(
            r#"tell application "iTerm2"
    set windowCount to count of windows
    repeat with i from 1 to windowCount
        set w to window i
        try
            set tabCount to count of tabs of w
            repeat with j from 1 to tabCount
                set t to tab j of w
                set s to current session of t
                try
                    set sName to name of s
                    if sName is equal to "{escaped_marker}" then
                        return id of w as text
                    end if
                end try
            end repeat
        end try
    end repeat
    return ""
end tell"#
        );
        let result = Self::run_applescript(&script).await?;
        if result.is_empty() {
            Ok(None)
        } else {
            Ok(Some(result))
        }
    }

    /// Find a session in a window by its display name.
    async fn find_session_by_name(
        window_id: &str,
        title: &str,
    ) -> PluginResult<Option<String>> {
        let escaped_title = title.replace('\\', "\\\\").replace('"', "\\\"");
        let script = format!(
            r#"tell application "iTerm2"
    try
        tell window id {window_id}
            set tabCount to count of tabs
            repeat with i from 1 to tabCount
                set t to tab i
                set s to current session of t
                try
                    set sName to name of s
                    if sName is equal to "{escaped_title}" then
                        return unique ID of s
                    end if
                end try
            end repeat
        end tell
    end try
    return ""
end tell"#
        );
        let result = Self::run_applescript(&script).await?;
        if result.is_empty() {
            Ok(None)
        } else {
            Ok(Some(result))
        }
    }

    /// Create a new iTerm2 window. Returns window ID as string.
    async fn create_window(group_key: &str) -> PluginResult<String> {
        let marker = format!("loopctl-{group_key}");
        let escaped_marker = marker.replace('"', "\\\"");
        let script = format!(
            r#"tell application "iTerm2"
    set newWindow to (create window with default profile)
    tell current session of current tab of newWindow
        set name to "{escaped_marker}"
    end tell
    return id of newWindow as text
end tell"#
        );
        Self::run_applescript(&script).await
    }

    /// Create a new tab in an existing window. Returns session unique ID.
    async fn create_tab_in_window(
        window_id: &str,
        workdir: &std::path::Path,
        title: &str,
        initial_command: Option<&str>,
    ) -> PluginResult<String> {
        let dir_str = workdir.to_string_lossy();
        let escaped_dir = dir_str.replace('\'', "'\\''");
        let escaped_title = title.replace('\\', "\\\\").replace('"', "\\\"");

        let cmd_line = match initial_command {
            Some(cmd) => {
                let escaped_cmd = cmd.replace('\'', "'\\''");
                format!("cd '{escaped_dir}' && {escaped_cmd}")
            }
            None => format!("cd '{escaped_dir}'"),
        };
        let escaped_cmd_line = cmd_line.replace('\\', "\\\\").replace('"', "\\\"");

        let script = format!(
            r#"tell application "iTerm2"
    tell window id {window_id}
        set newTab to (create tab with default profile)
        set s to current session of newTab
        set name of s to "{escaped_title}"
        tell s
            write text "{escaped_cmd_line}"
        end tell
        return unique ID of s
    end tell
end tell"#
        );
        Self::run_applescript(&script).await
    }
}

#[async_trait]
impl TerminalPlugin for Iterm2Plugin {
    fn name(&self) -> &'static str {
        "iterm2"
    }

    fn supports_environment(&self) -> Result<(), UnsupportedReason> {
        if std::env::consts::OS != "macos" {
            return Err(UnsupportedReason::WrongPlatform {
                backend: self.name(),
                detail: "iTerm2 is macOS-only".to_string(),
            });
        }
        if which::which("osascript").is_err() {
            return Err(UnsupportedReason::MissingDependency {
                backend: self.name(),
                requirement: "`osascript` on PATH".to_string(),
            });
        }
        Ok(())
    }

    async fn ensure_group(&self, group_key: &str) -> PluginResult<()> {
        // Check cache: verify the cached window still exists.
        let cached_wid = self.cache.read().groups.get(group_key).cloned();
        if let Some(wid) = cached_wid {
            let script = format!(
                r#"tell application "iTerm2"
    try
        set w to window id {wid}
        return "exists"
    on error
        return ""
    end try
end tell"#
            );
            let result = Self::run_applescript(&script).await?;
            if result == "exists" {
                return Ok(());
            }
            self.cache.write().groups.remove(group_key);
        }

        // Try to find an existing window tagged with our group marker.
        if let Some(wid) = Self::find_window_by_group(group_key).await? {
            self.cache
                .write()
                .groups
                .insert(group_key.to_string(), wid);
            return Ok(());
        }

        // Spawn a new window.
        let window_id = Self::create_window(group_key).await?;
        sleep(Duration::from_millis(500)).await;

        self.cache
            .write()
            .groups
            .insert(group_key.to_string(), window_id);
        Ok(())
    }

    async fn ensure_pane(&self, spec: &PaneSpec) -> PluginResult<PaneId> {
        self.ensure_group(&spec.group_key).await?;

        let window_id = self
            .cache
            .read()
            .groups
            .get(&spec.group_key)
            .cloned()
            .ok_or_else(|| {
                anyhow!(
                    "ensure_group did not populate cache for {}",
                    spec.group_key
                )
            })?;

        // Check if a session with this title already exists (idempotent).
        // iTerm2 may override the display name with the running process
        // name, so this is best-effort for fresh sessions.
        if let Some(session_id) =
            Self::find_session_by_name(&window_id, &spec.title).await?
        {
            return Ok(make_pane_id(&window_id, &session_id));
        }

        // Check if the parking tab (from ensure_group) still exists. Recycle it.
        let marker_title = format!("loopctl-{}", spec.group_key);
        if let Some(session_id) =
            Self::find_session_by_name(&window_id, &marker_title).await?
        {
            let dir_str = spec.workdir.to_string_lossy();
            let escaped_dir = dir_str.replace('\'', "'\\''");
            let escaped_title = spec.title.replace('\\', "\\\\").replace('"', "\\\"");

            let cmd_line = match &spec.initial_command {
                Some(cmd) => {
                    let escaped_cmd = cmd.replace('\'', "'\\''");
                    format!("cd '{escaped_dir}' && {escaped_cmd}")
                }
                None => format!("cd '{escaped_dir}'"),
            };
            let escaped_cmd_line = cmd_line.replace('\\', "\\\\").replace('"', "\\\"");

            let script = format!(
                r#"tell application "iTerm2"
    tell window id {window_id}
        set tabCount to count of tabs
        repeat with i from 1 to tabCount
            set t to tab i
            set s to current session of t
            try
                if (unique ID of s) is equal to "{session_id}" then
                    set name of s to "{escaped_title}"
                    tell s
                        write text "{escaped_cmd_line}"
                    end tell
                    exit repeat
                end if
            end try
        end repeat
    end tell
end tell"#
            );
            Self::run_applescript(&script).await?;
            return Ok(make_pane_id(&window_id, &session_id));
        }

        // Create a fresh tab.
        let session_id = Self::create_tab_in_window(
            &window_id,
            &spec.workdir,
            &spec.title,
            spec.initial_command.as_deref(),
        )
        .await?;

        Ok(make_pane_id(&window_id, &session_id))
    }

    async fn paste(&self, pane: &PaneId, text: &str) -> PluginResult<()> {
        let (window_id, session_id) = parse_pane_id(pane)?;

        let body = normalize_paste(text);
        let body = body.trim_end_matches('\n');

        if !body.is_empty() {
            let escaped_body = body.replace('\\', "\\\\").replace('"', "\\\"");
            let script = format!(
                r#"tell application "iTerm2"
    tell window id {window_id}
        set tabCount to count of tabs
        repeat with i from 1 to tabCount
            set t to tab i
            set s to current session of t
            try
                if (unique ID of s) is equal to "{session_id}" then
                    tell s
                        write text "{escaped_body}" without newline
                    end tell
                    exit repeat
                end if
            end try
        end repeat
    end tell
end tell"#
            );
            Self::run_applescript(&script).await?;
        }

        // Wait for claude to be ready, then send Enter.
        sleep(Duration::from_millis(self.paste_submit_delay_ms)).await;

        let script = format!(
            r#"tell application "iTerm2"
    tell window id {window_id}
        set tabCount to count of tabs
        repeat with i from 1 to tabCount
            set t to tab i
            set s to current session of t
            try
                if (unique ID of s) is equal to "{session_id}" then
                    tell s
                        write text ""
                    end tell
                    exit repeat
                end if
            end try
        end repeat
    end tell
end tell"#
        );
        Self::run_applescript(&script).await?;
        Ok(())
    }

    async fn run(&self, pane: &PaneId, command: &str) -> PluginResult<()> {
        let (window_id, session_id) = parse_pane_id(pane)?;
        let escaped_cmd = command.replace('\\', "\\\\").replace('"', "\\\"");
        let script = format!(
            r#"tell application "iTerm2"
    tell window id {window_id}
        set tabCount to count of tabs
        repeat with i from 1 to tabCount
            set t to tab i
            set s to current session of t
            try
                if (unique ID of s) is equal to "{session_id}" then
                    tell s
                        write text "{escaped_cmd}"
                    end tell
                    exit repeat
                end if
            end try
        end repeat
    end tell
end tell"#
        );
        Self::run_applescript(&script).await?;
        Ok(())
    }

    async fn focus(&self, pane: &PaneId) -> PluginResult<()> {
        let (window_id, session_id) = parse_pane_id(pane)?;
        let script = format!(
            r#"tell application "iTerm2"
    activate
    tell window id {window_id}
        set tabCount to count of tabs
        repeat with i from 1 to tabCount
            set t to tab i
            set s to current session of t
            try
                if (unique ID of s) is equal to "{session_id}" then
                    select t
                    exit repeat
                end if
            end try
        end repeat
    end tell
end tell"#
        );
        Self::run_applescript(&script).await?;
        Ok(())
    }

    async fn close(&self, pane: &PaneId) -> PluginResult<()> {
        let (window_id, session_id) = parse_pane_id(pane)?;
        let script = format!(
            r#"tell application "iTerm2"
    try
        tell window id {window_id}
            set tabCount to count of tabs
            repeat with i from 1 to tabCount
                set t to tab i
                set s to current session of t
                try
                    if (unique ID of s) is equal to "{session_id}" then
                        tell s to close
                        exit repeat
                    end if
                end try
            end repeat
        end tell
    end try
end tell"#
        );
        Self::run_applescript(&script).await?;
        Ok(())
    }

    async fn pane_alive(&self, pane: &PaneId) -> PluginResult<bool> {
        let (window_id, session_id) = parse_pane_id(pane)?;
        let script = format!(
            r#"tell application "iTerm2"
    try
        tell window id {window_id}
            set tabCount to count of tabs
            repeat with i from 1 to tabCount
                set t to tab i
                set s to current session of t
                try
                    if (unique ID of s) is equal to "{session_id}" then
                        return "alive"
                    end if
                end try
            end repeat
        end tell
    on error
        return "dead"
    end try
    return "dead"
end tell"#
        );
        let result = Self::run_applescript(&script).await?;
        Ok(result == "alive")
    }

    fn snapshot_groups(
        &self,
    ) -> std::collections::BTreeMap<String, crate::state::GroupState> {
        let mut out = std::collections::BTreeMap::new();
        for (group, window_id) in &self.cache.read().groups {
            out.insert(
                group.clone(),
                crate::state::GroupState {
                    pid: 0,
                    bus_name: window_id.clone(),
                    uuid: Some(format!("iterm2-{group}")),
                },
            );
        }
        out
    }

    fn forget_group(&self, group_key: &str) {
        self.cache.write().groups.remove(group_key);
    }

    async fn rehydrate(
        &self,
        previous: &std::collections::BTreeMap<String, crate::state::GroupState>,
    ) -> PluginResult<()> {
        for (group, gs) in previous {
            let window_id = &gs.bus_name;
            if window_id.is_empty() {
                continue;
            }
            let script = format!(
                r#"tell application "iTerm2"
    try
        set w to window id {window_id}
        return "exists"
    on error
        return ""
    end try
end tell"#
            );
            match Self::run_applescript(&script).await {
                Ok(result) if result == "exists" => {
                    self.cache
                        .write()
                        .groups
                        .insert(group.clone(), window_id.clone());
                    tracing::info!(
                        group = %group,
                        window_id = %window_id,
                        "rehydrated iTerm2 window for group"
                    );
                }
                _ => {
                    tracing::debug!(
                        group = %group,
                        window_id = %window_id,
                        "iTerm2 window gone; will spawn fresh on next ensure_group"
                    );
                }
            }
        }
        Ok(())
    }

    fn rekey_group(&self, old_key: &str, new_key: &str) {
        let mut c = self.cache.write();
        if let Some(v) = c.groups.remove(old_key) {
            c.groups.insert(new_key.to_string(), v);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pane_id_round_trip() {
        let id = make_pane_id("78467", "4E53E379-7A9E-47BB-9A05-94B11601362E");
        assert_eq!(
            id.as_str(),
            "iterm2:78467:4E53E379-7A9E-47BB-9A05-94B11601362E"
        );
        let (w, s) = parse_pane_id(&id).unwrap();
        assert_eq!(w, "78467");
        assert_eq!(s, "4E53E379-7A9E-47BB-9A05-94B11601362E");
    }

    #[test]
    fn pane_id_rejects_non_iterm2() {
        let bad = PaneId::new("konsole:1234:7");
        assert!(parse_pane_id(&bad).is_err());
    }
}