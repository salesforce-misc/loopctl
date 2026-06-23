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

//! Persistent daemon state stored at
//! `~/.local/state/agent-loop-workbench/state.json`.

pub mod model;

pub use model::{GlobalState, GroupState, LoopState, Metrics, PaneHealth};

use std::path::Path;

use anyhow::{Context, Result};

/// Load state from disk. Treats missing OR corrupt files as "fresh start"
/// so a partial write or a schema change never wedges the daemon.
/// Corrupt files get renamed with a `.bak.<unix-ts>` suffix for forensics.
pub fn load(path: &Path) -> Result<GlobalState> {
    let body = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(GlobalState::default());
        }
        Err(e) => {
            return Err(e).with_context(|| format!("read state.json at {path:?}"));
        }
    };
    match serde_json::from_str::<GlobalState>(&body) {
        Ok(s) => Ok(s),
        Err(e) => {
            let ts = chrono::Utc::now().timestamp();
            let bak = path.with_extension(format!("json.bak.{ts}"));
            tracing::warn!(
                ?path,
                ?bak,
                error = %e,
                "state.json is corrupt — backing up and starting fresh"
            );
            // Best-effort: don't fail the daemon if rename fails.
            let _ = std::fs::rename(path, &bak);
            Ok(GlobalState::default())
        }
    }
}

/// Atomic save. Writes to `<path>.tmp`, fsyncs, renames over `path`.
/// Mirrors the contract used by the user's existing `lib/loops/cache.py`.
pub fn save(path: &Path, state: &GlobalState) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("mkdir -p {parent:?}"))?;
    }
    let json = serde_json::to_string_pretty(state)?;
    let tmp = path.with_extension(format!(
        "{}.tmp",
        path.extension().and_then(|s| s.to_str()).unwrap_or("json")
    ));
    {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&tmp)
            .with_context(|| format!("open tmp {tmp:?}"))?;
        f.write_all(json.as_bytes())?;
        f.write_all(b"\n")?;
        f.sync_all().context("fsync tmp state.json")?;
    }
    std::fs::rename(&tmp, path)
        .with_context(|| format!("rename {tmp:?} -> {path:?}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::model::{GlobalState, GroupState, LoopState, Metrics, PaneHealth};
    use std::collections::BTreeMap;

    fn state_with_one_loop() -> GlobalState {
        let mut s = GlobalState::new("konsole");
        s.groups.insert(
            "2".into(),
            GroupState {
                pid: 12345,
                bus_name: "org.kde.konsole-12345".into(),
                uuid: Some("test-uuid".into()),
            },
        );
        let mut metrics = Metrics::default();
        metrics
            .integers
            .insert("tracker_rows".into(), 11);
        metrics.strings.insert(
            "next_target".into(),
            "investigate flaky test foo".into(),
        );
        s.loops.insert(
            "alpha-loop".into(),
            LoopState {
                pane_id: Some("konsole:12345:7".into()),
                last_tick: Some(chrono::Utc::now()),
                next_tick: None,
                last_dispatch_kind: Some("tick_script".into()),
                last_dispatch_exit: Some(0),
                last_metrics: metrics,
                pane_health: PaneHealth::Running,
            },
        );
        s
    }

    #[test]
    fn save_load_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let s1 = state_with_one_loop();
        save(&path, &s1).unwrap();
        let s2 = load(&path).unwrap();
        assert_eq!(s1.terminal, s2.terminal);
        assert_eq!(s1.groups.len(), s2.groups.len());
        assert_eq!(
            s1.loops.get("alpha-loop").unwrap().pane_id,
            s2.loops.get("alpha-loop").unwrap().pane_id
        );
    }

    #[test]
    fn missing_file_is_fresh_state() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nope.json");
        let s = load(&path).unwrap();
        assert_eq!(s.terminal, "");
        assert!(s.groups.is_empty());
        assert!(s.loops.is_empty());
    }

    #[test]
    fn corrupt_file_is_recovered_with_backup() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        std::fs::write(&path, "{ not valid json").unwrap();
        let s = load(&path).unwrap();
        assert!(s.groups.is_empty());
        // The corrupt original was renamed to *.bak.*
        let entries: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert!(
            entries.iter().any(|n| n.starts_with("state.json.bak.")),
            "expected a state.json.bak.* file, got {entries:?}"
        );
    }

    #[test]
    fn save_creates_missing_parent_dir() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/deeper/state.json");
        save(&path, &GlobalState::new("konsole")).unwrap();
        assert!(path.exists());
    }

    #[test]
    fn save_overwrites_atomically() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let s1 = state_with_one_loop();
        save(&path, &s1).unwrap();
        let mut s2 = s1.clone();
        s2.loops.get_mut("alpha-loop").unwrap().last_dispatch_exit = Some(7);
        save(&path, &s2).unwrap();
        let loaded = load(&path).unwrap();
        assert_eq!(
            loaded
                .loops
                .get("alpha-loop")
                .unwrap()
                .last_dispatch_exit,
            Some(7)
        );
        // No leftover .tmp
        let entries: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert!(
            !entries.iter().any(|n| n.contains(".tmp")),
            "leftover tmp file: {entries:?}"
        );
    }

    #[test]
    fn use_btreemap_for_unused_lint_silencer() {
        // Touch BTreeMap so the import isn't unused if we later remove a test
        let _: BTreeMap<String, String> = BTreeMap::new();
    }
}