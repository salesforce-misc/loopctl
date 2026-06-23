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

//! User-level config at `~/.config/loopctl/config.toml`.
//!
//! Stores defaults that aren't tied to any one loops repo — chiefly
//! `claude_loops_dir`, the path to the repo whose `workbench.toml`
//! `loopctl` should load by default.

use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct UserConfig {
    /// Default claude_loops_dir, e.g. `~/claude-loops`. The `workbench.toml`
    /// inside this dir is what `loopctl daemon`/`status`/`tui` etc. load
    /// when the user doesn't pass `--config`.
    #[serde(default)]
    pub claude_loops_dir: Option<String>,

    /// If true, after every config-mutating RPC the daemon `git push`es
    /// the canonical workbench.toml's containing repo. Default false:
    /// auto-commit always happens locally, push is opt-in. Pair with
    /// `git init` + `git remote add origin …` in `~/.config/loopctl/`.
    #[serde(default)]
    pub loopctl_git_auto_push: bool,
}

/// `~/.config/loopctl/config.toml` (or `$XDG_CONFIG_HOME/loopctl/config.toml`).
pub fn user_config_path() -> Result<PathBuf> {
    if let Ok(s) = std::env::var("XDG_CONFIG_HOME") {
        if !s.is_empty() {
            return Ok(PathBuf::from(s).join("loopctl/config.toml"));
        }
    }
    let home = std::env::var("HOME").context("HOME must be set")?;
    Ok(PathBuf::from(home).join(".config/loopctl/config.toml"))
}

/// Load the user config. Missing file ⇒ defaults; corrupt file ⇒ error.
pub fn load_user_config() -> Result<UserConfig> {
    let path = user_config_path()?;
    if !path.exists() {
        return Ok(UserConfig::default());
    }
    let body = std::fs::read_to_string(&path)
        .with_context(|| format!("read user config at {path:?}"))?;
    let cfg: UserConfig =
        toml::from_str(&body).with_context(|| format!("parse user config at {path:?}"))?;
    Ok(cfg)
}

/// Atomic save to user config path. Creates parent dir if needed.
pub fn save_user_config(cfg: &UserConfig) -> Result<()> {
    let path = user_config_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("mkdir -p {parent:?}"))?;
    }
    let body = toml::to_string_pretty(cfg)?;
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, body).with_context(|| format!("write {tmp:?}"))?;
    std::fs::rename(&tmp, &path).with_context(|| format!("rename to {path:?}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// These tests mutate process-wide env vars so they must not run in
    /// parallel with each other. We use a shared mutex to serialize them.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    // SAFETY for the env::set_var / remove_var calls below:
    // edition-2024 made these unsafe because they race with concurrent
    // readers on glibc. The ENV_LOCK mutex above serializes every test
    // in this module, and these tests are the only env mutators in the
    // file — so no concurrent reader exists at the moments we mutate.

    #[test]
    fn round_trip_through_temp_xdg_home() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let prev = std::env::var("XDG_CONFIG_HOME").ok();
        unsafe { std::env::set_var("XDG_CONFIG_HOME", dir.path()) };

        let cfg = UserConfig {
            claude_loops_dir: Some("~/some-other-loops".into()),
            loopctl_git_auto_push: false,
        };
        save_user_config(&cfg).unwrap();
        let loaded = load_user_config().unwrap();
        assert_eq!(loaded.claude_loops_dir.as_deref(), Some("~/some-other-loops"));

        match prev {
            Some(v) => unsafe { std::env::set_var("XDG_CONFIG_HOME", v) },
            None => unsafe { std::env::remove_var("XDG_CONFIG_HOME") },
        }
    }

    #[test]
    fn missing_file_returns_default() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let prev = std::env::var("XDG_CONFIG_HOME").ok();
        unsafe { std::env::set_var("XDG_CONFIG_HOME", dir.path()) };

        let cfg = load_user_config().unwrap();
        assert!(cfg.claude_loops_dir.is_none());

        match prev {
            Some(v) => unsafe { std::env::set_var("XDG_CONFIG_HOME", v) },
            None => unsafe { std::env::remove_var("XDG_CONFIG_HOME") },
        }
    }
}