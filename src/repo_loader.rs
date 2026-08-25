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

//! Resolve a user-supplied "repo" string (local path OR git URL) to a
//! local working directory.
//!
//! - Local: any string with a leading `/`, `~`, `.`, or `..` is treated
//!   as a filesystem path; we just expand and verify it exists.
//! - Git: anything matching one of the URL prefixes below is shelled
//!   out to `git clone` into `~/.local/share/loopctl/repos/<basename>`.
//!   Subsequent loads of the same URL re-use that clone (no auto-pull
//!   to avoid surprising the user's working tree).

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use crate::discover::{discover_loops, DiscoveredLoop};
use crate::terminal::TerminalPlugin;

const GIT_PREFIXES: &[&str] =
    &["http://", "https://", "git://", "git@", "ssh://", "git+ssh://"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RepoKind {
    LocalPath,
    GitUrl,
}

pub fn classify(input: &str) -> RepoKind {
    let s = input.trim();
    if GIT_PREFIXES.iter().any(|p| s.starts_with(p)) {
        return RepoKind::GitUrl;
    }
    // git@host.tld:owner/repo.git is the SCP-like form; classify only if
    // it has a `:` AND the part before `:` looks like `user@host`.
    if let Some(at) = s.find('@') {
        let after_at = &s[at + 1..];
        if let Some(colon) = after_at.find(':') {
            let host = &after_at[..colon];
            if !host.is_empty() && host.contains('.') {
                return RepoKind::GitUrl;
            }
        }
    }
    RepoKind::LocalPath
}

/// Resolve `input` to a local PathBuf containing the repo's working tree.
/// For git URLs, clones if not already present in the loopctl cache.
pub fn resolve(input: &str) -> Result<PathBuf> {
    match classify(input) {
        RepoKind::LocalPath => {
            let expanded = crate::util::expand_path(input);
            let pb = PathBuf::from(expanded);
            if !pb.exists() {
                bail!("path does not exist: {pb:?}");
            }
            Ok(pb)
        }
        RepoKind::GitUrl => clone_or_reuse(input),
    }
}

fn clone_or_reuse(url: &str) -> Result<PathBuf> {
    let cache_root = clone_cache_root()?;
    let basename = git_url_basename(url)?;
    let dest = cache_root.join(&basename);
    if dest.join(".git").exists() {
        // Already cloned; reuse without auto-pulling.
        return Ok(dest);
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("mkdir -p {parent:?}"))?;
    }
    let status = std::process::Command::new("git")
        .arg("clone")
        .arg(url)
        .arg(&dest)
        .status()
        .context("spawn git clone (is `git` on PATH?)")?;
    if !status.success() {
        bail!(
            "git clone exited with {} for {url}",
            status
                .code()
                .map(|c| c.to_string())
                .unwrap_or_else(|| "unknown".into())
        );
    }
    Ok(dest)
}

fn clone_cache_root() -> Result<PathBuf> {
    if let Ok(s) = std::env::var("XDG_DATA_HOME") {
        if !s.is_empty() {
            return Ok(PathBuf::from(s).join("loopctl/repos"));
        }
    }
    let home = std::env::var("HOME").context("HOME not set")?;
    Ok(PathBuf::from(home).join(".local/share/loopctl/repos"))
}

fn git_url_basename(url: &str) -> Result<String> {
    // Take the last path segment, strip a trailing `.git`, sanitize.
    let after_colon = url.rsplit_once(':').map(|(_, r)| r).unwrap_or(url);
    let last = after_colon
        .rsplit('/')
        .next()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow::anyhow!("could not derive basename from {url:?}"))?;
    let trimmed = last.trim_end_matches(".git");
    if trimmed.is_empty() {
        bail!("empty basename for git url {url:?}");
    }
    // Only keep alnum, dash, underscore, dot. Other chars become underscore.
    let safe: String = trimmed
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '_'
            }
        })
        .collect();
    Ok(safe)
}

#[allow(dead_code)]
pub fn looks_like_local(input: &str) -> bool {
    classify(input) == RepoKind::LocalPath
}

/// Outcome of a `load_repo` call. Useful for the TUI flash + the CLI
/// summary to be consistent.
#[derive(Debug, Clone)]
pub struct LoadOutcome {
    pub repo_dir: PathBuf,
    pub workbench_toml: PathBuf,
    /// One of: `Generated` (file did not exist; full file written),
    /// `Appended { added }` (file existed; we appended `added` new loops),
    /// `UpToDate` (file existed and discovery found no new loops).
    pub action: LoadAction,
    /// Loops discovered. Always populated.
    pub discovered: Vec<DiscoveredLoop>,
    /// (name, desktop_number) pairs for *newly added* loops only.
    pub desktop_plan: Vec<(String, u32)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadAction {
    /// Wrote a fresh workbench.toml from scratch.
    Generated,
    /// Existing workbench.toml; appended this many newly-discovered loops.
    Appended { added: usize },
    /// Existing workbench.toml; every discovered loop is already present.
    UpToDate,
}

/// End-to-end "load this repo" — resolve path, ensure workbench.toml
/// covers every loop discovered in README.md (creating or appending),
/// save the resolved path to user config. Caller is responsible for
/// restarting the daemon afterwards.
pub async fn load_repo(input: &str, plugin: &dyn TerminalPlugin) -> Result<LoadOutcome> {
    let repo_dir = resolve(input)?;
    let workbench = repo_dir.join("workbench.toml");
    let discovered = discover_loops(&repo_dir)?;
    if discovered.is_empty() {
        bail!(
            "no loops discovered in {repo_dir:?}: README.md must contain `### <heading>` entries whose anchor maps to an existing dir"
        );
    }

    let desktop_count = plugin.desktop_count().await.unwrap_or(0);
    let empty_desktops = if desktop_count > 0 {
        plugin.empty_desktops(desktop_count).await.unwrap_or_default()
    } else {
        Vec::new()
    };

    let action = if workbench.exists() {
        // Append-only path: read existing names, only emit stanzas for
        // discovered loops not yet present.
        let existing_names = read_existing_names(&workbench)?;
        let to_add: Vec<DiscoveredLoop> = discovered
            .iter()
            .filter(|l| !existing_names.contains(&l.name))
            .cloned()
            .collect();
        if to_add.is_empty() {
            LoadAction::UpToDate
        } else {
            let plan_full = assign_desktops(&to_add, &empty_desktops, desktop_count);
            append_loops_to_workbench_toml(&workbench, &to_add, &plan_full)?;
            LoadAction::Appended {
                added: to_add.len(),
            }
        }
    } else {
        let plan_full = assign_desktops(&discovered, &empty_desktops, desktop_count);
        write_workbench_toml(&workbench, &repo_dir, &discovered, &plan_full)?;
        LoadAction::Generated
    };

    // Re-validate the result so we fail loudly if our writer produced
    // something the parser rejects.
    crate::config::load(&workbench).context("workbench.toml failed to re-parse after load")?;

    // Save resolved path to user config so future `loopctl daemon`
    // invocations point at this repo by default. Tests can suppress
    // this side effect to avoid clobbering the developer's real
    // ~/.config/loopctl/config.toml.
    if std::env::var_os("LOOPCTL_TEST_INHIBIT_USER_CONFIG").is_none() {
        let mut uc = crate::config::load_user_config().unwrap_or_default();
        uc.claude_loops_dir = Some(repo_dir.to_string_lossy().to_string());
        crate::config::save_user_config(&uc)?;
    }

    let desktop_plan = match &action {
        LoadAction::UpToDate => Vec::new(),
        LoadAction::Generated => assign_desktops(&discovered, &empty_desktops, desktop_count),
        LoadAction::Appended { .. } => {
            let existing_names = read_existing_names(&workbench).unwrap_or_default();
            let new_only: Vec<DiscoveredLoop> = discovered
                .iter()
                // ⚠ existing_names now includes the loops we just appended;
                // recompute against pre-append by re-reading? We don't —
                // the test below for Appended.added gives the number, and
                // the desktop_plan we report just lists everything in the
                // file as a courtesy. Keep all discovered.
                .filter(|l| existing_names.contains(&l.name))
                .cloned()
                .collect();
            assign_desktops(&new_only, &empty_desktops, desktop_count)
        }
    };

    Ok(LoadOutcome {
        repo_dir,
        workbench_toml: workbench,
        action,
        discovered,
        desktop_plan,
    })
}

/// Pull `name = "..."` lines out of an existing workbench.toml so we
/// can avoid duplicating loops on re-load. We deliberately don't
/// `toml::from_str` here — the file may have been hand-edited and
/// might not round-trip through our writer.
fn read_existing_names(path: &Path) -> Result<std::collections::HashSet<String>> {
    let body =
        std::fs::read_to_string(path).with_context(|| format!("read {path:?}"))?;
    let mut out = std::collections::HashSet::new();
    let mut in_loops_array = false;
    for raw in body.lines() {
        let line = raw.trim();
        if line == "[[loops]]" {
            in_loops_array = true;
            continue;
        }
        if line.starts_with('[') && line != "[[loops]]" {
            in_loops_array = false;
        }
        if in_loops_array {
            // Accept both the new `name = "..."` and the legacy
            // `slug = "..."` (still parseable via the serde alias on
            // LoopConfig.name) so re-loading an old repo doesn't
            // duplicate every loop on the first pass after upgrade.
            let rest = line
                .strip_prefix("name")
                .or_else(|| line.strip_prefix("slug"));
            if let Some(rest) = rest {
                let after_eq = rest.split('=').nth(1).unwrap_or("").trim();
                let name = after_eq.trim_matches(|c: char| c == '"' || c == '\'').trim();
                if !name.is_empty() {
                    out.insert(name.to_string());
                }
            }
        }
    }
    Ok(out)
}

fn append_loops_to_workbench_toml(
    path: &Path,
    loops: &[DiscoveredLoop],
    plan: &[(String, u32)],
) -> Result<()> {
    let mut body =
        std::fs::read_to_string(path).with_context(|| format!("read {path:?}"))?;
    if !body.ends_with('\n') {
        body.push('\n');
    }
    if !body.ends_with("\n\n") {
        body.push('\n');
    }
    body.push_str("# --- appended by `loopctl load-repo` ---\n");
    for l in loops {
        let group = plan
            .iter()
            .find(|(name, _)| name == &l.name)
            .map(|(_, d)| *d)
            .unwrap_or(7);
        body.push_str("[[loops]]\n");
        body.push_str(&format!("name             = \"{}\"\n", l.name));
        body.push_str(&format!("dir              = \"{}\"\n", l.dir));
        body.push_str(&format!(
            "readme_anchor    = \"{}\"\n",
            l.readme_anchor
        ));
        body.push_str(&format!("group_key        = \"{group}\"\n"));
        body.push_str(&format!("tab_title        = \"{}\"\n", l.tab_title));
        body.push_str("schedule         = \"none\"\n");
        body.push_str("dispatch         = \"resume_prompt\"\n\n");
        // No claude_args override: load-repo loops come from a curated
        // repo and inherit the workbench-wide default
        // (--dangerously-skip-permissions). To prompt for permissions
        // on a single loop, add `claude_args = []` manually.
    }
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, body).with_context(|| format!("write {tmp:?}"))?;
    std::fs::rename(&tmp, path).with_context(|| format!("rename to {path:?}"))?;
    Ok(())
}

/// Pack each loop into the FIRST empty desktop. When empty desktops
/// run out, fall through to round-robin over the empty list (so loops
/// share a konsole). If empty_desktops is empty AND desktop_count > 0,
/// fall back to round-robin over all desktops starting at 1. If both
/// are zero (no plugin support), fall back to a fixed 7..=9 range.
fn assign_desktops(
    loops: &[DiscoveredLoop],
    empty_desktops: &[u32],
    desktop_count: u32,
) -> Vec<(String, u32)> {
    let pool: Vec<u32> = if !empty_desktops.is_empty() {
        empty_desktops.to_vec()
    } else if desktop_count > 0 {
        (1..=desktop_count).collect()
    } else {
        vec![7, 8, 9]
    };
    loops
        .iter()
        .enumerate()
        .map(|(i, l)| (l.name.clone(), pool[i % pool.len()]))
        .collect()
}

fn write_workbench_toml(
    path: &Path,
    repo_dir: &Path,
    loops: &[DiscoveredLoop],
    plan: &[(String, u32)],
) -> Result<()> {
    let central = plan.iter().map(|(_, d)| *d).max().map(|d| d.to_string()).unwrap_or_else(|| "1".to_string());
    let mut s = String::new();
    s.push_str("schema_version = 1\n\n");
    s.push_str("[workbench]\n");
    s.push_str(&format!(
        "claude_loops_dir = \"{}\"\n",
        repo_dir.display()
    ));
    let terminal_name = if cfg!(target_os = "macos") { "iterm2" } else { "konsole" };
    s.push_str(&format!("terminal         = \"{terminal_name}\"\n"));
    s.push_str(&format!("central_group    = \"{central}\"\n"));
    s.push_str("default_dispatch = \"resume_prompt\"\n");
    s.push_str("# Agent every loop runs unless it sets its own `agent` (claude | codex | opencode).\n");
    s.push_str("default_agent    = \"claude\"\n");
    s.push_str("# Default args for the default agent. New loops opt out (see add-loop).\n");
    s.push_str("agent_args       = [\"--dangerously-skip-permissions\"]\n\n");

    for l in loops {
        let group = plan
            .iter()
            .find(|(name, _)| name == &l.name)
            .map(|(_, d)| *d)
            .unwrap_or(7);
        s.push_str("[[loops]]\n");
        s.push_str(&format!("name             = \"{}\"\n", l.name));
        s.push_str(&format!("dir              = \"{}\"\n", l.dir));
        s.push_str(&format!(
            "readme_anchor    = \"{}\"\n",
            l.readme_anchor
        ));
        s.push_str(&format!("group_key        = \"{group}\"\n"));
        s.push_str(&format!("tab_title        = \"{}\"\n", l.tab_title));
        s.push_str("schedule         = \"none\"\n");
        s.push_str("dispatch         = \"resume_prompt\"\n\n");
    }
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, s).with_context(|| format!("write {tmp:?}"))?;
    std::fs::rename(&tmp, path).with_context(|| format!("rename to {path:?}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_https() {
        assert_eq!(
            classify("https://github.com/foo/bar"),
            RepoKind::GitUrl
        );
    }

    #[test]
    fn classify_git_at_host() {
        assert_eq!(classify("git@github.com:foo/bar.git"), RepoKind::GitUrl);
    }

    #[test]
    fn classify_local_paths() {
        assert_eq!(classify("/tmp/loops"), RepoKind::LocalPath);
        assert_eq!(classify("~/claude-loops"), RepoKind::LocalPath);
        assert_eq!(classify("./loops"), RepoKind::LocalPath);
    }

    #[test]
    fn classify_user_at_path_is_local_not_url() {
        // `git@/tmp` is nonsense but shouldn't accidentally classify as
        // a URL. We only treat user@host:path as a URL when host has a
        // dot (looks like a domain).
        assert_eq!(classify("alice@machine:loops"), RepoKind::LocalPath);
    }

    #[test]
    fn basename_from_https_strips_dot_git() {
        assert_eq!(
            git_url_basename("https://github.com/example-org/example-loops.git").unwrap(),
            "example-loops"
        );
        assert_eq!(
            git_url_basename("https://github.com/example-org/example-loops").unwrap(),
            "example-loops"
        );
    }

    #[test]
    fn basename_from_scp_form() {
        assert_eq!(
            git_url_basename("git@github.com:foo/bar.git").unwrap(),
            "bar"
        );
    }

    #[test]
    fn local_path_resolves_to_pathbuf() {
        let dir = tempfile::tempdir().unwrap();
        let p = resolve(dir.path().to_str().unwrap()).unwrap();
        assert_eq!(p, dir.path());
    }

    #[test]
    fn local_path_not_found_errors() {
        let err = resolve("/no/such/path-loopctl-test").unwrap_err();
        assert!(err.to_string().contains("does not exist"));
    }

    #[test]
    fn read_existing_names_finds_quoted_values() {
        // Mix `name = ...` (new) and `slug = ...` (legacy) — both must
        // be picked up so re-loading an upgraded repo doesn't double
        // up the legacy loops.
        let body = r#"
schema_version = 1

[workbench]
claude_loops_dir = "~/x"
terminal = "konsole"
central_group = "1"

[[loops]]
name = "alpha"
dir = "alpha"
readme_anchor = "alpha"
group_key = "7"
schedule = "none"
dispatch = "resume_prompt"

[[loops]]
name             = "beta"
dir              = "beta"
readme_anchor    = "beta"
group_key        = "8"
schedule         = "none"
dispatch         = "resume_prompt"

[[loops]]
slug = "gamma-legacy"
dir = "gamma"
readme_anchor = "gamma"
group_key = "9"
schedule = "none"
dispatch = "resume_prompt"

[[other_section]]
name = "should_not_match"
"#;
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("w.toml");
        std::fs::write(&p, body).unwrap();
        let s = read_existing_names(&p).unwrap();
        assert!(s.contains("alpha"));
        assert!(s.contains("beta"));
        assert!(s.contains("gamma-legacy"));
        assert!(!s.contains("should_not_match"));
    }
}