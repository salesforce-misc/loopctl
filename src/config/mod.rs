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

//! `workbench.toml` schema + loader, plus the user-level
//! `~/.config/loopctl/config.toml` that points at it.

pub mod schema;
pub mod user;

pub use schema::{
    builtin_agent_preset, AgentOverride, Dispatch, KonsoleSettings, LoopConfig, ResolvedAgent,
    Schedule, TerminalSettings, WorkbenchConfig, WorkbenchSettings, BUILTIN_AGENTS,
};
pub use user::{load_user_config, save_user_config, user_config_path, UserConfig};

use std::path::Path;

use anyhow::{Context, Result};

/// Read and parse a workbench.toml from disk.
pub fn load(path: &Path) -> Result<WorkbenchConfig> {
    let body = std::fs::read_to_string(path)
        .with_context(|| format!("read workbench.toml at {path:?}"))?;
    parse(&body).with_context(|| format!("parse workbench.toml at {path:?}"))
}

/// Parse a workbench.toml from an in-memory string.
pub fn parse(body: &str) -> Result<WorkbenchConfig> {
    let cfg: WorkbenchConfig = toml::from_str(body)?;
    cfg.validate()?;
    Ok(cfg)
}

/// After a config-mutating write, snapshot the change in git so we have
/// history + a recovery point. If `~/.config/loopctl/` is not a git
/// repo, no-op (we don't auto-`git init` for the user — too magical).
/// If `loopctl_git_auto_push = true` in the user config, also push.
///
/// Errors here are logged but never propagated: a successful workbench
/// edit must not fail because git is wedged.
pub fn snapshot_in_git(config_path: &Path, message: &str) {
    let Some(parent) = config_path.parent() else {
        return;
    };
    if !parent.join(".git").exists() {
        return;
    }
    let Some(file_name) = config_path.file_name().and_then(|s| s.to_str()) else {
        return;
    };
    // git add <file>; git diff --cached --quiet; if changed, git commit.
    let add = std::process::Command::new("git")
        .args(["add", "--", file_name])
        .current_dir(parent)
        .status();
    if !matches!(add, Ok(s) if s.success()) {
        tracing::warn!(?config_path, "snapshot_in_git: git add failed");
        return;
    }
    let staged_changes = std::process::Command::new("git")
        .args(["diff", "--cached", "--quiet", "--", file_name])
        .current_dir(parent)
        .status();
    // exit 0 = no changes; exit 1 = changes; anything else = error.
    let has_changes = matches!(&staged_changes, Ok(s) if s.code() == Some(1));
    if !has_changes {
        return;
    }
    let commit = std::process::Command::new("git")
        .args(["commit", "--quiet", "-m", message])
        .current_dir(parent)
        .status();
    if !matches!(commit, Ok(s) if s.success()) {
        tracing::warn!(?config_path, "snapshot_in_git: git commit failed");
        return;
    }
    tracing::info!(?config_path, msg = %message, "config change committed");

    // Opt-in push. We re-read user config every call rather than
    // caching: the user might toggle the flag without restarting
    // loopctl, and missing one push is fine but skipping intent isn't.
    let auto_push = load_user_config()
        .ok()
        .map(|uc| uc.loopctl_git_auto_push)
        .unwrap_or(false);
    if !auto_push {
        return;
    }
    let push = std::process::Command::new("git")
        .args(["push", "--quiet"])
        .current_dir(parent)
        .status();
    if !matches!(push, Ok(s) if s.success()) {
        tracing::warn!(?config_path, "snapshot_in_git: git push failed (continuing)");
    }
}