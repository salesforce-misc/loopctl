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

//! Resolve XDG state / runtime / config paths for the workbench.

use std::path::PathBuf;

use anyhow::{anyhow, Context, Result};
use directories::ProjectDirs;

const QUALIFIER: &str = "";
const ORG: &str = "";
const APP: &str = "agent-loop-workbench";

fn project_dirs() -> Result<ProjectDirs> {
    ProjectDirs::from(QUALIFIER, ORG, APP)
        .ok_or_else(|| anyhow!("could not resolve XDG project dirs for {APP}"))
}

/// `~/.local/state/agent-loop-workbench/` (or `XDG_STATE_HOME`).
pub fn state_dir() -> Result<PathBuf> {
    // ProjectDirs::data_local_dir() is the closest match in `directories`;
    // fall back to a manual XDG_STATE_HOME read because `directories` does
    // not expose a state dir on Linux.
    if let Ok(s) = std::env::var("XDG_STATE_HOME") {
        if !s.is_empty() {
            return Ok(PathBuf::from(s).join(APP));
        }
    }
    let home =
        std::env::var("HOME").context("HOME must be set to resolve state dir")?;
    Ok(PathBuf::from(home).join(".local/state").join(APP))
}

/// `~/.local/state/agent-loop-workbench/state.json`.
pub fn state_file() -> Result<PathBuf> {
    Ok(state_dir()?.join("state.json"))
}

/// `~/.local/state/agent-loop-workbench/loopctl.log`.
pub fn log_file() -> Result<PathBuf> {
    Ok(state_dir()?.join("loopctl.log"))
}

/// `$XDG_RUNTIME_DIR/agent-loop-workbench.sock`.
pub fn ipc_socket() -> Result<PathBuf> {
    let runtime = std::env::var("XDG_RUNTIME_DIR").or_else(|_| {
        // Fallback: /tmp/agent-loop-workbench-<uid>.sock.
        let uid = nix::unistd::getuid().as_raw();
        Ok::<String, std::env::VarError>(format!("/tmp/loopctl-{uid}"))
    })?;
    let pb = PathBuf::from(runtime);
    Ok(pb.join("agent-loop-workbench.sock"))
}

/// XDG config dir; not yet used (workbench.toml lives in claude_loops_dir).
pub fn config_dir() -> Result<PathBuf> {
    Ok(project_dirs()?.config_dir().to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_dir_respects_xdg_state_home() {
        // SAFETY: edition-2024 made set_var/remove_var unsafe because they
        // race with concurrent readers on glibc. This test is the only env
        // mutator in this file; cargo runs tests within a single target
        // serially-ish per thread, but to be safe we'd want a mutex shared
        // with other env-mutating tests. For now: this is the only one.
        let prev = std::env::var("XDG_STATE_HOME").ok();
        unsafe { std::env::set_var("XDG_STATE_HOME", "/tmp/my-state") };
        let p = state_dir().unwrap();
        assert_eq!(p, PathBuf::from("/tmp/my-state/agent-loop-workbench"));
        match prev {
            Some(v) => unsafe { std::env::set_var("XDG_STATE_HOME", v) },
            None => unsafe { std::env::remove_var("XDG_STATE_HOME") },
        }
    }
}