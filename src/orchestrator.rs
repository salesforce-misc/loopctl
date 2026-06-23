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

//! Pure orchestration: walk a WorkbenchConfig and drive a TerminalPlugin
//! to bring up / tear down panes. The daemon's IPC handler calls into
//! these functions; integration tests call them directly with a mock
//! plugin to assert op ordering.

use std::path::Path;
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};

use crate::config::{LoopConfig, WorkbenchConfig};
use crate::readme;
use crate::state::{GlobalState, LoopState, PaneHealth};
use crate::terminal::{PaneSpec, TerminalPlugin};

/// Bring all configured loops up.
///
/// 1. Group ensured before any pane in that group.
/// 2. Panes ensured in config declaration order within a group.
/// 3. For each new pane (no existing `pane_id` in state), paste the resume
///    prompt extracted from `claude_loops_dir/README.md`.
///
/// The mock plugin re-records all ops so tests can assert the sequence.
///
/// On a per-loop failure (e.g. missing workdir, README anchor not
/// found), the loop's state is marked `FailedRecreate` and the sweep
/// continues with the rest. Returning early on the first failure
/// would leave subsequent loops in `Unknown` even though they're
/// fine — surprising behavior when you've got 7 loops and one is
/// misconfigured.
///
/// If any loop failed, the function still returns Err at the end so
/// the caller can surface that to the user — but every other loop has
/// been brought up by then.
pub async fn up(
    config: &WorkbenchConfig,
    plugin: Arc<dyn TerminalPlugin>,
    state: &mut GlobalState,
) -> Result<()> {
    let mut failures: Vec<(String, String)> = Vec::new();
    // Iterate in config order; grouping is implicit because ensure_group is
    // idempotent. This preserves declaration ordering for the contract test.
    for lc in &config.loops {
        if !lc.active {
            // Inactive loops are skipped entirely. Mark state to match
            // so the TUI shows them as inactive instead of stale.
            let entry = state.loops.entry(lc.name.clone()).or_default();
            entry.pane_health = PaneHealth::Inactive {
                since: chrono::Utc::now(),
            };
            entry.pane_id = None;
            continue;
        }
        if let Err(e) = bring_up_loop(config, plugin.as_ref(), state, lc).await {
            tracing::warn!(name = %lc.name, error = %e, "bring_up_loop failed; continuing with remaining loops");
            // Mirror what handle_dead_pane would do: surface the
            // failure on the loop's health so the TUI shows it.
            let entry = state.loops.entry(lc.name.clone()).or_default();
            entry.pane_id = None;
            entry.pane_health = PaneHealth::FailedRecreate {
                since: chrono::Utc::now(),
                attempts: 1,
                last_error: format!("{e:#}"),
            };
            failures.push((lc.name.clone(), format!("{e:#}")));
        }
    }
    if !failures.is_empty() {
        let summary: Vec<String> = failures
            .into_iter()
            .map(|(n, e)| format!("`{n}`: {e}"))
            .collect();
        anyhow::bail!(
            "{} loop(s) failed to come up:\n  {}",
            summary.len(),
            summary.join("\n  ")
        );
    }
    Ok(())
}

/// Bring up a single loop. Idempotent: if the loop already has a healthy
/// pane (matching pane_id in state), this is a no-op.
pub async fn up_one(
    config: &WorkbenchConfig,
    plugin: &dyn TerminalPlugin,
    state: &mut GlobalState,
    name: &str,
) -> Result<()> {
    let lc = config
        .loop_by_name(name)
        .ok_or_else(|| anyhow!("unknown loop `{name}`"))?;
    if !lc.active {
        anyhow::bail!(
            "loop `{name}` is inactive — run `loopctl reopen {name}` to reactivate it"
        );
    }
    bring_up_loop(config, plugin, state, lc).await
}

async fn bring_up_loop(
    config: &WorkbenchConfig,
    plugin: &dyn TerminalPlugin,
    state: &mut GlobalState,
    lc: &LoopConfig,
) -> Result<()> {
    let loops_dir = crate::util::expand_path(&config.workbench.claude_loops_dir);
    let loops_dir = Path::new(&loops_dir);
    let readme_path = loops_dir.join("README.md");

    // If state already has a pane_id for this loop and the pane is still
    // alive, skip ensure_pane entirely. This avoids creating duplicate
    // tabs when the terminal backend can't find existing sessions by name
    // (e.g. iTerm2 overrides the session name with the running process).
    if let Some(existing_id) = state
        .loops
        .get(&lc.name)
        .and_then(|s| s.pane_id.clone())
    {
        let existing_pane = crate::terminal::PaneId(existing_id.clone());
        if let Ok(true) = plugin.pane_alive(&existing_pane).await {
            let entry = state.loops.entry(lc.name.clone()).or_default();
            entry.pane_health = PaneHealth::Running;
            return Ok(());
        }
    }

    plugin.ensure_group(&lc.group_key).await?;
    let pane_id = ensure_pane_for_loop(plugin, config, lc, loops_dir).await?;
    let already_had_pane_id = state
        .loops
        .get(&lc.name)
        .and_then(|s| s.pane_id.as_ref())
        .map(|s| s == pane_id.as_str())
        .unwrap_or(false);

    let entry = state.loops.entry(lc.name.clone()).or_default();
    entry.pane_id = Some(pane_id.0.clone());
    entry.pane_health = PaneHealth::Running;

    if !already_had_pane_id {
        // First time we ensured this pane — paste the resume prompt.
        let prompt = readme::extract_from_path(&readme_path, &lc.readme_anchor)
            .with_context(|| {
                format!(
                    "extract resume prompt for `{}` (anchor: {})",
                    lc.name, lc.readme_anchor
                )
            })?;
        plugin.paste(&pane_id, &prompt).await.with_context(|| {
            format!("paste resume prompt into pane for loop `{}`", lc.name)
        })?;
    }
    Ok(())
}

async fn ensure_pane_for_loop(
    plugin: &dyn TerminalPlugin,
    config: &WorkbenchConfig,
    lc: &LoopConfig,
    loops_dir: &Path,
) -> Result<crate::terminal::PaneId> {
    let workdir = loops_dir.join(&lc.dir);
    if !workdir.exists() {
        return Err(anyhow!(
            "loop `{}` workdir does not exist: {workdir:?}",
            lc.name
        ));
    }
    let initial = Some(claude_invocation(config, lc));
    let spec = PaneSpec {
        group_key: lc.group_key.clone(),
        title: lc.effective_tab_title().to_string(),
        workdir,
        initial_command: initial,
    };
    plugin.ensure_pane(&spec).await
}

/// Build the shell line that the konsole tab will run on launch:
/// `<claude_binary> <claude_args...>` with each arg POSIX-quoted.
fn claude_invocation(config: &WorkbenchConfig, lc: &LoopConfig) -> String {
    let mut s = posix_quote(&config.workbench.claude_binary);
    for a in lc.effective_claude_args(&config.workbench) {
        s.push(' ');
        s.push_str(&posix_quote(a));
    }
    s
}

fn posix_quote(s: &str) -> String {
    if !s.is_empty()
        && s.chars().all(|c| {
            c.is_ascii_alphanumeric()
                || matches!(c, '_' | '-' | '.' | '/' | ':' | '+' | '@' | ',' | '=')
        })
    {
        return s.to_string();
    }
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Close every pane the daemon owns. Leaves the central group's pane
/// alone (the workbench TUI). Called by the `down` RPC.
pub async fn down(
    config: &WorkbenchConfig,
    plugin: Arc<dyn TerminalPlugin>,
    state: &mut GlobalState,
) -> Result<()> {
    for lc in &config.loops {
        if lc.group_key == config.workbench.central_group {
            continue;
        }
        let Some(LoopState { pane_id: Some(id), .. }) = state.loops.get(&lc.name) else {
            continue;
        };
        let pane = crate::terminal::PaneId(id.clone());
        if let Err(e) = plugin.close(&pane).await {
            tracing::warn!(name = %lc.name, error = %e, "close pane failed");
        }
        if let Some(s) = state.loops.get_mut(&lc.name) {
            s.pane_id = None;
        }
    }
    Ok(())
}

/// One pass of pane health checks. For every loop with a `pane_id` in
/// state:
///   - probe the plugin; if alive, mark Running.
///   - if dead and current health was Running/Unknown, transition to
///     Crashed{since=now}.
///   - if dead and was already Crashed/FailedRecreate, attempt
///     `up_one`; on success transition to Running, on failure record
///     FailedRecreate{attempts++}.
///
/// Crashed → recreate-attempt happens in the SAME pass. We don't wait
/// for a future tick — the user wants the konsole back, fast.
pub async fn health_check_and_recover(
    config: &WorkbenchConfig,
    plugin: Arc<dyn TerminalPlugin>,
    state: &mut GlobalState,
) -> Result<()> {
    let now = chrono::Utc::now();
    // Pull the name list up-front so we don't borrow state while
    // calling into the plugin (which is .await-heavy). Includes loops
    // currently in Crashed/FailedRecreate state (they have no pane_id
    // but still need recovery attempts).
    let names: Vec<String> = state
        .loops
        .iter()
        .filter_map(|(s, ls)| {
            if ls.pane_id.is_some() || ls.pane_health.needs_recreate() {
                Some(s.clone())
            } else {
                None
            }
        })
        .collect();

    for name in names {
        // Snapshot what we need; release the borrow before awaiting.
        let (pane_id_str, prior_health) = {
            let ls = match state.loops.get(&name) {
                Some(ls) => ls,
                None => continue,
            };
            (
                ls.pane_id.clone().unwrap_or_default(),
                ls.pane_health.clone(),
            )
        };
        // No pane_id but needs recreate: jump straight into the dead
        // branch using a synthetic "dead" probe outcome.
        if pane_id_str.is_empty() {
            handle_dead_pane(config, plugin.as_ref(), state, &name, prior_health, now).await;
            continue;
        }
        let pane = crate::terminal::PaneId(pane_id_str);

        match plugin.pane_alive(&pane).await {
            Ok(true) => {
                if let Some(ls) = state.loops.get_mut(&name) {
                    ls.pane_health = PaneHealth::Running;
                }
            }
            Ok(false) => {
                handle_dead_pane(config, plugin.as_ref(), state, &name, prior_health, now)
                    .await;
            }
            Err(e) => {
                // Transient probe error — leave health untouched, log.
                tracing::debug!(name = %name, error = %e, "pane_alive probe errored; leaving health untouched");
            }
        }
    }
    Ok(())
}

async fn handle_dead_pane(
    config: &WorkbenchConfig,
    plugin: &dyn TerminalPlugin,
    state: &mut GlobalState,
    name: &str,
    prior_health: PaneHealth,
    now: chrono::DateTime<chrono::Utc>,
) {
    let lc = config.loop_by_name(name);
    // If the loop is configured inactive, don't try to revive it.
    if let Some(c) = lc {
        if !c.active {
            let entry = state.loops.entry(name.to_string()).or_default();
            entry.pane_id = None;
            entry.pane_health = PaneHealth::Inactive { since: now };
            return;
        }
    }
    // Mark crashed first so the state reflects what we saw, even if
    // recovery later fails.
    {
        let entry = state.loops.entry(name.to_string()).or_default();
        entry.pane_id = None;
        entry.pane_health = match &prior_health {
            PaneHealth::Crashed { since } => PaneHealth::Crashed { since: *since },
            PaneHealth::FailedRecreate { since, .. } => PaneHealth::Crashed { since: *since },
            _ => PaneHealth::Crashed { since: now },
        };
    }
    // NOTE: do NOT call `plugin.forget_group(g)` here. The plugin's
    // own `ensure_group` already verifies via `cached_alive` whether
    // the konsole is alive, and evicts the cache if not. Forcing a
    // forget here means that when several loops in the same group are
    // killed in one batch, EACH triggers a fresh konsole spawn — even
    // if a sibling already brought one up. Letting ensure_group
    // self-evict gives us "spawn at most once per dead group."
    if lc.is_none() {
        return; // configured-out loop; leave name crashed
    }
    let attempt_n = match prior_health {
        PaneHealth::FailedRecreate { attempts, .. } => attempts + 1,
        _ => 1,
    };
    tracing::info!(name = %name, attempt = attempt_n, "attempting pane recreate");
    match up_one(config, plugin, state, name).await {
        Ok(()) => {
            tracing::info!(name = %name, "pane recreated");
        }
        Err(e) => {
            tracing::warn!(name = %name, error = %e, "pane recreate failed");
            let entry = state.loops.entry(name.to_string()).or_default();
            entry.pane_id = None;
            let since = match &entry.pane_health {
                PaneHealth::Crashed { since } => *since,
                PaneHealth::FailedRecreate { since, .. } => *since,
                _ => now,
            };
            entry.pane_health = PaneHealth::FailedRecreate {
                since,
                attempts: attempt_n,
                last_error: format!("{e:#}"),
            };
        }
    }
}

/// Replace the README's resume-prompt body for `name`. If `repaste`
/// is true and the loop has a live pane, the new prompt is also pasted
/// into that pane (so claude sees the update without a manual restart).
pub async fn update_prompt(
    config: &WorkbenchConfig,
    plugin: &dyn TerminalPlugin,
    state: &GlobalState,
    name: &str,
    new_body: &str,
    repaste: bool,
) -> Result<()> {
    let lc = config
        .loop_by_name(name)
        .ok_or_else(|| anyhow!("unknown loop `{name}`"))?;
    let loops_dir = crate::util::expand_path(&config.workbench.claude_loops_dir);
    let readme = std::path::Path::new(&loops_dir).join("README.md");
    crate::readme::replace_resume_prompt_in_file(&readme, &lc.readme_anchor, new_body)?;

    if repaste {
        if let Some(pane_id) = state
            .loops
            .get(name)
            .and_then(|s| s.pane_id.clone())
        {
            let pane = crate::terminal::PaneId(pane_id);
            plugin
                .paste(&pane, new_body)
                .await
                .with_context(|| format!("paste new prompt into pane for `{name}`"))?;
        }
    }
    Ok(())
}

/// Permanently delete a loop:
///   1. close any live pane (SIGHUP via the plugin),
///   2. drop the entry from workbench.toml,
///   3. strip the loop's `### <heading>` block from
///      `<claude_loops_dir>/README.md` so re-load doesn't rediscover it,
///   4. drop the in-memory `state.loops[name]` and persisted state,
///   5. `rm -rf <claude_loops_dir>/<dir>` (the loop directory).
///
/// This is destructive — the caller must have already obtained user
/// confirmation. Returns a [`DeleteSummary`] describing what was done
/// so the CLI can show a precise after-action report.
pub async fn delete_loop(
    config: &mut WorkbenchConfig,
    config_path: &std::path::Path,
    plugin: Arc<dyn TerminalPlugin>,
    state: &mut GlobalState,
    name: &str,
) -> Result<DeleteSummary> {
    let lc_idx = config
        .loops
        .iter()
        .position(|l| l.name == name)
        .ok_or_else(|| anyhow!("unknown loop `{name}`"))?;
    let lc = config.loops[lc_idx].clone();

    let mut summary = DeleteSummary {
        name: name.to_string(),
        ..Default::default()
    };

    // Step 1: close the live pane if any. Best-effort — if the pane is
    // already gone we don't want to abort the rest of the delete.
    if let Some(pane_id_str) = state
        .loops
        .get(name)
        .and_then(|s| s.pane_id.clone())
    {
        let pane = crate::terminal::PaneId(pane_id_str.clone());
        if let Err(e) = plugin.close(&pane).await {
            tracing::warn!(name, error = %e, "close pane during delete failed; continuing");
        } else {
            summary.pane_closed = Some(pane_id_str);
        }
    }

    // Step 2: rewrite workbench.toml without this loop.
    rewrite_workbench_remove(config_path, name)?;
    config.loops.remove(lc_idx);
    summary.removed_from_config = true;

    // Step 3: strip the README block. Best-effort — if the heading isn't
    // present (manually deleted, or the loop was added without one), we
    // log and continue.
    let loops_dir =
        std::path::PathBuf::from(crate::util::expand_path(&config.workbench.claude_loops_dir));
    let readme_path = loops_dir.join("README.md");
    match crate::readme::strip_readme_block_in_file(&readme_path, &lc.readme_anchor) {
        Ok(true) => summary.readme_block_stripped = true,
        Ok(false) => {
            tracing::info!(
                name,
                anchor = %lc.readme_anchor,
                "no README block matched the loop's anchor; skipping that step"
            );
        }
        Err(e) => {
            tracing::warn!(name, error = %e, "stripping README block failed; continuing");
        }
    }

    // Step 4: state cleanup.
    state.loops.remove(name);
    summary.state_dropped = true;

    // Step 5: rm -rf the loop dir. Refuse if it would try to delete
    // something outside `claude_loops_dir` (defense against a config
    // typo like `dir = "../.."` or, more dangerously, a symlink that
    // points outside the loops dir). We canonicalize both sides so
    // symlink trickery is resolved before the prefix check; if
    // canonicalization fails (path missing) we fall back to the
    // lexical comparison and refuse on any doubt.
    let loop_dir = loops_dir.join(&lc.dir);
    if !is_inside(&loops_dir, &loop_dir) {
        tracing::warn!(
            name,
            ?loop_dir,
            ?loops_dir,
            "refusing to rm-rf a path outside claude_loops_dir"
        );
    } else if loop_dir.exists() {
        if let Err(e) = std::fs::remove_dir_all(&loop_dir) {
            tracing::warn!(name, ?loop_dir, error = %e, "failed to remove loop dir");
        } else {
            summary.dir_removed = Some(loop_dir.display().to_string());
        }
    }

    Ok(summary)
}

/// Returns true iff `child` resolves to a path inside `parent`. Uses
/// `canonicalize` to follow symlinks before comparing; if either side
/// can't be canonicalized (e.g. path doesn't exist) the function
/// requires that the *lexical* form already starts with the parent's
/// canonicalized path. Defaults to refusing on any ambiguity.
pub fn is_inside(parent: &Path, child: &Path) -> bool {
    let parent_canon = match std::fs::canonicalize(parent) {
        Ok(p) => p,
        Err(_) => return false,
    };
    match std::fs::canonicalize(child) {
        Ok(c) => c.starts_with(&parent_canon),
        Err(_) => {
            // Child doesn't exist on disk yet (or is a broken symlink).
            // Fall back to a lexical check against the canonical parent.
            child.starts_with(&parent_canon) && !child.components().any(|c| {
                matches!(c, std::path::Component::ParentDir)
            })
        }
    }
}

#[derive(Debug, Default)]
pub struct DeleteSummary {
    pub name: String,
    pub pane_closed: Option<String>,
    pub removed_from_config: bool,
    pub readme_block_stripped: bool,
    pub state_dropped: bool,
    pub dir_removed: Option<String>,
}

/// Re-write `workbench.toml` without the given loop. Mutates via the
/// parsed config so we keep TOML integrity (no line-edit hacks).
fn rewrite_workbench_remove(path: &std::path::Path, name: &str) -> Result<()> {
    let mut cfg = crate::config::load(path)
        .with_context(|| format!("re-load workbench.toml at {path:?}"))?;
    cfg.loops.retain(|l| l.name != name);
    let body = toml::to_string_pretty(&cfg)?;
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, body).with_context(|| format!("write {tmp:?}"))?;
    std::fs::rename(&tmp, path).with_context(|| format!("rename to {path:?}"))?;
    crate::config::snapshot_in_git(path, &format!("loopctl: delete-loop {name}"));
    Ok(())
}

/// Mark a loop inactive: rewrite workbench.toml with `active = false`,
/// close any live pane, set state to `Inactive`. Idempotent — closing
/// a loop that's already inactive is a no-op.
pub async fn close_loop(
    config: &mut WorkbenchConfig,
    config_path: &std::path::Path,
    plugin: Arc<dyn TerminalPlugin>,
    state: &mut GlobalState,
    name: &str,
) -> Result<()> {
    let lc = config
        .loop_by_name(name)
        .ok_or_else(|| anyhow!("unknown loop `{name}`"))?;
    let already_inactive = !lc.active;

    if !already_inactive {
        rewrite_workbench_active(config_path, name, false)?;
        if let Some(lc_mut) = config.loops.iter_mut().find(|l| l.name == name) {
            lc_mut.active = false;
        }
    }

    // Close the live pane regardless of prior config state — the user
    // wants a clean transition.
    if let Some(pane_id_str) = state
        .loops
        .get(name)
        .and_then(|s| s.pane_id.clone())
    {
        let pane = crate::terminal::PaneId(pane_id_str);
        if let Err(e) = plugin.close(&pane).await {
            tracing::warn!(name, error = %e, "close pane during close_loop failed");
        }
    }

    let entry = state.loops.entry(name.to_string()).or_default();
    entry.pane_id = None;
    entry.next_tick = None;
    entry.pane_health = PaneHealth::Inactive {
        since: chrono::Utc::now(),
    };
    Ok(())
}

/// Reactivate a closed loop: rewrite workbench.toml with `active = true`,
/// then bring it up immediately (so the user sees a pane right away).
/// Idempotent — reopening an active loop just runs `up_one`.
pub async fn reopen_loop(
    config: &mut WorkbenchConfig,
    config_path: &std::path::Path,
    plugin: Arc<dyn TerminalPlugin>,
    state: &mut GlobalState,
    name: &str,
) -> Result<()> {
    let lc = config
        .loop_by_name(name)
        .ok_or_else(|| anyhow!("unknown loop `{name}`"))?;
    if !lc.active {
        rewrite_workbench_active(config_path, name, true)?;
        if let Some(lc_mut) = config.loops.iter_mut().find(|l| l.name == name) {
            lc_mut.active = true;
        }
    }
    // Reset health so up_one writes a clean Running.
    if let Some(s) = state.loops.get_mut(name) {
        s.pane_health = PaneHealth::Unknown;
    }
    up_one(config, plugin.as_ref(), state, name).await
}

fn rewrite_workbench_active(
    path: &std::path::Path,
    name: &str,
    active: bool,
) -> Result<()> {
    let mut cfg = crate::config::load(path)
        .with_context(|| format!("re-load workbench.toml at {path:?}"))?;
    let lc = cfg
        .loops
        .iter_mut()
        .find(|l| l.name == name)
        .ok_or_else(|| anyhow!("workbench.toml has no loop `{name}`"))?;
    lc.active = active;
    let body = toml::to_string_pretty(&cfg)?;
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, body).with_context(|| format!("write {tmp:?}"))?;
    std::fs::rename(&tmp, path).with_context(|| format!("rename to {path:?}"))?;
    let verb = if active { "reopen" } else { "close" };
    crate::config::snapshot_in_git(path, &format!("loopctl: {verb}-loop {name}"));
    Ok(())
}

/// Outcome of a move-group request.
pub enum MoveGroupOutcome {
    /// Move completed.
    Moved,
    /// Move would be destructive (multi-tab konsole) and `force` was
    /// false. The daemon refuses; the caller can re-call with force=true.
    RequiresConfirm { tabs_in_group: usize },
}

/// Change the `group_key` of `name` to `new_group`. Persists
/// `workbench.toml`. Behavior depends on the live state:
///
/// - If the loop has no live pane (Crashed/Unknown), just rewrite the
///   config and let the next health pass spawn it on the new desktop.
/// - If the loop is the *only* tab in its konsole, move the whole
///   window via the plugin's `move_group_to_desktop` (Konsole/X11
///   only — works because the window IS the group).
/// - If the loop is one of multiple tabs and `force == false`, return
///   `RequiresConfirm`. With `force == true`, close the tab and call
///   `up_one` in the new group — claude restarts.
pub async fn move_group(
    config: &mut WorkbenchConfig,
    config_path: &std::path::Path,
    plugin: Arc<dyn TerminalPlugin>,
    state: &mut GlobalState,
    name: &str,
    new_group: &str,
    force: bool,
) -> Result<MoveGroupOutcome> {
    let lc = config
        .loop_by_name(name)
        .ok_or_else(|| anyhow!("unknown loop `{name}`"))?;
    let old_group = lc.group_key.clone();
    if old_group == new_group {
        return Ok(MoveGroupOutcome::Moved);
    }

    // Count tabs that share the OLD group with this loop. If we're the
    // last tab there, the whole window can move atomically.
    let tabs_in_group = config
        .loops
        .iter()
        .filter(|l| l.group_key == old_group)
        .count();

    let pane_id_now = state
        .loops
        .get(name)
        .and_then(|s| s.pane_id.clone());

    if pane_id_now.is_some() && tabs_in_group > 1 && !force {
        return Ok(MoveGroupOutcome::RequiresConfirm { tabs_in_group });
    }

    // Update config in memory and on disk.
    rewrite_workbench_group(config_path, name, new_group)?;
    if let Some(lc_mut) = config.loops.iter_mut().find(|l| l.name == name) {
        lc_mut.group_key = new_group.to_string();
    }

    if let Some(pane_id) = pane_id_now {
        if tabs_in_group == 1 {
            // Whole window can move via the plugin's per-group desktop op.
            // For Konsole, this means: re-tag (no-op if uuid known) and
            // ask KWin to put the window on the new desktop number.
            if let Err(e) = plugin
                .move_group_to(&old_group, new_group)
                .await
            {
                tracing::warn!(
                    error = %e,
                    "move_group_to failed; will fall through to recreate path"
                );
                close_pane_and_respawn(plugin.as_ref(), state, &pane_id, config, name).await?;
            } else {
                // Update group_key in cache-keyed structures.
                plugin.rekey_group(&old_group, new_group);
                // Pane survives; nothing else to record.
            }
        } else {
            // force == true here. Close the tab and re-spawn in the new
            // group. claude restarts with the resume prompt.
            close_pane_and_respawn(plugin.as_ref(), state, &pane_id, config, name).await?;
        }
    } else {
        // No live pane — bring it up under the new group now.
        let _ = up_one(config, plugin.as_ref(), state, name).await;
    }

    Ok(MoveGroupOutcome::Moved)
}

async fn close_pane_and_respawn(
    plugin: &dyn TerminalPlugin,
    state: &mut GlobalState,
    pane_id_str: &str,
    config: &WorkbenchConfig,
    name: &str,
) -> Result<()> {
    let pane = crate::terminal::PaneId(pane_id_str.to_string());
    let _ = plugin.close(&pane).await;
    if let Some(s) = state.loops.get_mut(name) {
        s.pane_id = None;
        s.pane_health = PaneHealth::Crashed {
            since: chrono::Utc::now(),
        };
    }
    up_one(config, plugin, state, name).await
}

fn rewrite_workbench_group(
    path: &std::path::Path,
    name: &str,
    new_group: &str,
) -> Result<()> {
    // Load → mutate → write. We re-parse the TOML and walk the loops
    // array because line-edits would risk corrupting comments. The
    // round-trip will drop comments and reformat, but that's acceptable
    // for an interactive operation.
    let mut cfg = crate::config::load(path)
        .with_context(|| format!("re-load workbench.toml at {path:?}"))?;
    let lc = cfg
        .loops
        .iter_mut()
        .find(|l| l.name == name)
        .ok_or_else(|| anyhow!("workbench.toml has no loop `{name}`"))?;
    lc.group_key = new_group.to_string();
    let body = toml::to_string_pretty(&cfg)?;
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, body).with_context(|| format!("write {tmp:?}"))?;
    std::fs::rename(&tmp, path).with_context(|| format!("rename to {path:?}"))?;
    crate::config::snapshot_in_git(
        path,
        &format!("loopctl: move-group {name} -> {new_group}"),
    );
    Ok(())
}

/// Focus a pane (KDE switches desktop + raises window for that pane).
pub async fn attach(
    config: &WorkbenchConfig,
    plugin: Arc<dyn TerminalPlugin>,
    state: &GlobalState,
    name: &str,
) -> Result<()> {
    let _lc = config
        .loop_by_name(name)
        .ok_or_else(|| anyhow!("unknown loop `{name}`"))?;
    let id = state
        .loops
        .get(name)
        .and_then(|s| s.pane_id.clone())
        .ok_or_else(|| anyhow!("loop `{name}` has no pane (run `loopctl up` first)"))?;
    plugin.focus(&crate::terminal::PaneId(id)).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config;
    use crate::terminal::mock::{MockTerminalPlugin, Op};

    fn write_min_loops_dir(root: &Path, anchors_to_loops: &[(&str, &str)]) {
        std::fs::create_dir_all(root).unwrap();
        let mut readme = String::from("# header\n\n");
        for (anchor_text, dir_name) in anchors_to_loops {
            std::fs::create_dir_all(root.join(dir_name)).unwrap();
            readme.push_str(&format!("\n### {anchor_text}\n\n```\nresume {dir_name}\n```\n"));
        }
        std::fs::write(root.join("README.md"), readme).unwrap();
    }

    fn cfg_for(loops_dir: &Path) -> WorkbenchConfig {
        let body = format!(
            r#"
schema_version = 1

[workbench]
claude_loops_dir = "{}"
terminal = "konsole"
central_group = "1"

[[loops]]
name = "alpha"
dir = "alpha-dir"
readme_anchor = "alpha--label"
group_key = "2"
schedule = "none"
dispatch = "resume_prompt"

[[loops]]
name = "beta"
dir = "beta-dir"
readme_anchor = "beta--label"
group_key = "2"
schedule = "none"
dispatch = "resume_prompt"

[[loops]]
name = "gamma"
dir = "gamma-dir"
readme_anchor = "gamma--label"
group_key = "3"
schedule = "none"
dispatch = "resume_prompt"
"#,
            loops_dir.display()
        );
        config::parse(&body).expect("parse cfg")
    }

    #[test]
    fn claude_invocation_uses_workbench_default_when_loop_unset() {
        let mut cfg = config::parse(
            r#"
schema_version = 1

[workbench]
claude_loops_dir = "/tmp"
terminal = "konsole"
central_group = "1"

[[loops]]
name = "alpha"
dir = "alpha-dir"
readme_anchor = "alpha"
group_key = "2"
schedule = "none"
dispatch = "resume_prompt"
"#,
        )
        .unwrap();
        // workbench-wide default applies when LoopConfig.claude_args is None.
        let alpha = &cfg.loops[0];
        assert_eq!(
            super::claude_invocation(&cfg, alpha),
            "claude --dangerously-skip-permissions"
        );

        // Explicit override at workbench level.
        cfg.workbench.claude_args = vec!["--allowed-tools".into(), "Read,Write".into()];
        assert_eq!(
            super::claude_invocation(&cfg, alpha),
            "claude --allowed-tools Read,Write"
        );
    }

    #[test]
    fn claude_invocation_loop_override_wins_and_empty_means_no_args() {
        let cfg = config::parse(
            r#"
schema_version = 1

[workbench]
claude_loops_dir = "/tmp"
terminal = "konsole"
central_group = "1"

[[loops]]
name = "alpha"
dir = "alpha-dir"
readme_anchor = "alpha"
group_key = "2"
schedule = "none"
dispatch = "resume_prompt"
claude_args = []  # explicit opt-out — new untrusted loop

[[loops]]
name = "beta"
dir = "beta-dir"
readme_anchor = "beta"
group_key = "2"
schedule = "none"
dispatch = "resume_prompt"
claude_args = ["--allowed-tools", "Read"]
"#,
        )
        .unwrap();
        let alpha = cfg.loop_by_name("alpha").unwrap();
        let beta = cfg.loop_by_name("beta").unwrap();
        assert_eq!(super::claude_invocation(&cfg, alpha), "claude");
        assert_eq!(
            super::claude_invocation(&cfg, beta),
            "claude --allowed-tools Read"
        );
    }

    #[tokio::test]
    async fn up_ensures_groups_then_panes_in_declared_order() {
        let dir = tempfile::tempdir().unwrap();
        write_min_loops_dir(
            dir.path(),
            &[
                ("alpha — label", "alpha-dir"),
                ("beta — label", "beta-dir"),
                ("gamma — label", "gamma-dir"),
            ],
        );
        let cfg = cfg_for(dir.path());
        let plugin = Arc::new(MockTerminalPlugin::new());
        let mut state = GlobalState::default();
        up(&cfg, plugin.clone(), &mut state).await.unwrap();

        let ops = plugin.ops();
        // Find positions of EnsureGroup("2") and the first EnsurePane in group 2
        let i_g2 = ops
            .iter()
            .position(|op| matches!(op, Op::EnsureGroup(k) if k == "2"))
            .unwrap();
        let i_p_alpha = ops
            .iter()
            .position(|op| matches!(op, Op::EnsurePane { title, .. } if title == "alpha"))
            .unwrap();
        let i_p_beta = ops
            .iter()
            .position(|op| matches!(op, Op::EnsurePane { title, .. } if title == "beta"))
            .unwrap();
        let i_p_gamma = ops
            .iter()
            .position(|op| matches!(op, Op::EnsurePane { title, .. } if title == "gamma"))
            .unwrap();
        assert!(i_g2 < i_p_alpha);
        assert!(i_p_alpha < i_p_beta);
        // gamma is in a different group; its position is irrelevant relative to alpha/beta
        assert!(i_p_gamma > i_g2 || i_p_gamma > i_p_alpha);

        // All 3 panes should have had a resume prompt pasted on first up.
        let pastes: Vec<_> = ops
            .iter()
            .filter(|op| matches!(op, Op::Paste { .. }))
            .collect();
        assert_eq!(pastes.len(), 3);
    }

    #[tokio::test]
    async fn second_up_does_not_paste_again() {
        let dir = tempfile::tempdir().unwrap();
        // cfg_for declares 3 loops; create all 3 dirs.
        write_min_loops_dir(
            dir.path(),
            &[
                ("alpha — label", "alpha-dir"),
                ("beta — label", "beta-dir"),
                ("gamma — label", "gamma-dir"),
            ],
        );
        let cfg = cfg_for(dir.path());
        let plugin = Arc::new(MockTerminalPlugin::new());
        let mut state = GlobalState::default();
        up(&cfg, plugin.clone(), &mut state).await.unwrap();
        let pastes_first: usize = plugin
            .ops()
            .iter()
            .filter(|op| matches!(op, Op::Paste { .. }))
            .count();
        assert_eq!(pastes_first, 3);

        // Second up: panes already exist (idempotent), pane_id matches state,
        // so no fresh paste should fire.
        up(&cfg, plugin.clone(), &mut state).await.unwrap();
        let pastes_total: usize = plugin
            .ops()
            .iter()
            .filter(|op| matches!(op, Op::Paste { .. }))
            .count();
        assert_eq!(
            pastes_total, 3,
            "resume prompt must NOT be re-pasted on subsequent up"
        );
    }

    #[tokio::test]
    async fn missing_loop_dir_errors_with_helpful_message() {
        let dir = tempfile::tempdir().unwrap();
        // Create the README but NOT the loop dirs
        std::fs::write(
            dir.path().join("README.md"),
            "### alpha — label\n\n```\nx\n```\n",
        )
        .unwrap();
        let cfg = cfg_for(dir.path());
        let plugin = Arc::new(MockTerminalPlugin::new());
        let mut state = GlobalState::default();
        let err = up(&cfg, plugin, &mut state).await.unwrap_err().to_string();
        assert!(err.contains("workdir does not exist"));
    }

    #[tokio::test]
    async fn up_continues_past_failing_loop_and_marks_it_failed_recreate() {
        // Three loops in cfg_for (alpha/beta/gamma). Create dirs for
        // alpha + gamma but NOT beta. The sweep should bring up alpha
        // and gamma, mark beta FailedRecreate, then return Err that
        // names beta.
        let dir = tempfile::tempdir().unwrap();
        let mut readme = String::from("# header\n\n");
        for (anchor_text, dir_name) in [
            ("alpha — label", "alpha-dir"),
            // Intentionally skip beta-dir creation.
            ("gamma — label", "gamma-dir"),
        ] {
            std::fs::create_dir_all(dir.path().join(dir_name)).unwrap();
            readme.push_str(&format!("\n### {anchor_text}\n\n```\nresume {dir_name}\n```\n"));
        }
        // Add beta's heading so the README parser finds it (the dir is
        // what's missing).
        readme.push_str("\n### beta — label\n\n```\nresume beta-dir\n```\n");
        std::fs::write(dir.path().join("README.md"), readme).unwrap();

        let cfg = cfg_for(dir.path());
        let plugin = Arc::new(MockTerminalPlugin::new());
        let mut state = GlobalState::default();
        let err = up(&cfg, plugin, &mut state).await.unwrap_err().to_string();
        // Aggregated error names beta but not alpha/gamma.
        assert!(err.contains("`beta`"), "got: {err}");
        assert!(err.contains("workdir does not exist"));
        // alpha + gamma still got panes.
        assert!(state.loops.get("alpha").unwrap().pane_id.is_some());
        assert!(state.loops.get("gamma").unwrap().pane_id.is_some());
        // beta is FailedRecreate.
        match &state.loops.get("beta").unwrap().pane_health {
            PaneHealth::FailedRecreate { last_error, .. } => {
                assert!(last_error.contains("workdir does not exist"));
            }
            other => panic!("expected FailedRecreate, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn down_closes_non_central_panes_only() {
        let dir = tempfile::tempdir().unwrap();
        write_min_loops_dir(
            dir.path(),
            &[
                ("alpha — label", "alpha-dir"),
                ("beta — label", "beta-dir"),
                ("gamma — label", "gamma-dir"),
            ],
        );
        // Stage the central loop on group "1" so down() should skip it.
        let mut cfg = cfg_for(dir.path());
        cfg.loops[0].group_key = "1".into(); // alpha → central

        let plugin = Arc::new(MockTerminalPlugin::new());
        let mut state = GlobalState::default();
        up(&cfg, plugin.clone(), &mut state).await.unwrap();
        let ops_before = plugin.ops().len();
        down(&cfg, plugin.clone(), &mut state).await.unwrap();
        let new_closes: Vec<_> = plugin
            .ops()
            .into_iter()
            .skip(ops_before)
            .filter(|op| matches!(op, Op::Close(_)))
            .collect();
        assert_eq!(new_closes.len(), 2, "alpha (central) must NOT be closed");
        assert!(state.loops.get("alpha").unwrap().pane_id.is_some()); // untouched
        assert!(state.loops.get("beta").unwrap().pane_id.is_none());
        assert!(state.loops.get("gamma").unwrap().pane_id.is_none());
    }

    #[tokio::test]
    async fn attach_unknown_loop_errors() {
        let dir = tempfile::tempdir().unwrap();
        write_min_loops_dir(dir.path(), &[("alpha — label", "alpha-dir")]);
        let cfg = cfg_for(dir.path());
        let plugin = Arc::new(MockTerminalPlugin::new());
        let state = GlobalState::default();
        let err = attach(&cfg, plugin, &state, "ghost").await.unwrap_err();
        assert!(err.to_string().contains("unknown loop"));
    }

    #[tokio::test]
    async fn attach_without_pane_errors() {
        let dir = tempfile::tempdir().unwrap();
        write_min_loops_dir(dir.path(), &[("alpha — label", "alpha-dir")]);
        let cfg = cfg_for(dir.path());
        let plugin = Arc::new(MockTerminalPlugin::new());
        let state = GlobalState::default();
        let err = attach(&cfg, plugin, &state, "alpha").await.unwrap_err();
        assert!(err.to_string().contains("no pane"));
    }

    #[tokio::test]
    async fn delete_loop_removes_config_state_pane_and_dir() {
        let dir = tempfile::tempdir().unwrap();
        write_min_loops_dir(
            dir.path(),
            &[
                ("alpha — label", "alpha-dir"),
                ("beta — label", "beta-dir"),
                ("gamma — label", "gamma-dir"),
            ],
        );
        // Persist a real config file so rewrite_workbench_remove can
        // round-trip through it.
        let cfg_path = dir.path().join("workbench.toml");
        let cfg_str = format!(
            r#"
schema_version = 1

[workbench]
claude_loops_dir = "{}"
terminal = "konsole"
central_group = "1"

[[loops]]
name = "alpha"
dir = "alpha-dir"
readme_anchor = "alpha--label"
group_key = "2"
schedule = "none"
dispatch = "resume_prompt"

[[loops]]
name = "beta"
dir = "beta-dir"
readme_anchor = "beta--label"
group_key = "2"
schedule = "none"
dispatch = "resume_prompt"

[[loops]]
name = "gamma"
dir = "gamma-dir"
readme_anchor = "gamma--label"
group_key = "3"
schedule = "none"
dispatch = "resume_prompt"
"#,
            dir.path().display()
        );
        std::fs::write(&cfg_path, &cfg_str).unwrap();
        let mut cfg = crate::config::parse(&cfg_str).unwrap();

        let plugin = Arc::new(MockTerminalPlugin::new());
        let mut state = GlobalState::default();
        up(&cfg, plugin.clone(), &mut state).await.unwrap();

        // Sanity pre-check: beta has a pane and beta-dir exists.
        assert!(state.loops.get("beta").unwrap().pane_id.is_some());
        assert!(dir.path().join("beta-dir").exists());

        let summary =
            delete_loop(&mut cfg, &cfg_path, plugin.clone(), &mut state, "beta")
                .await
                .unwrap();
        assert_eq!(summary.name, "beta");
        assert!(summary.removed_from_config);
        assert!(summary.state_dropped);
        assert!(summary.readme_block_stripped);
        assert!(summary.dir_removed.is_some(), "loop dir should be removed");
        assert!(summary.pane_closed.is_some(), "pane should be closed");

        // Config no longer has beta.
        assert_eq!(cfg.loops.iter().filter(|l| l.name == "beta").count(), 0);
        // Persisted file matches.
        let on_disk = std::fs::read_to_string(&cfg_path).unwrap();
        assert!(!on_disk.contains("name = \"beta\""));
        // README beta block stripped.
        let readme = std::fs::read_to_string(dir.path().join("README.md")).unwrap();
        assert!(!readme.contains("beta — label"));
        // beta-dir gone.
        assert!(!dir.path().join("beta-dir").exists());
        // alpha + gamma untouched.
        assert!(state.loops.get("alpha").unwrap().pane_id.is_some());
        assert!(state.loops.get("gamma").unwrap().pane_id.is_some());
        assert!(dir.path().join("alpha-dir").exists());
        assert!(dir.path().join("gamma-dir").exists());
    }

    #[tokio::test]
    async fn delete_loop_refuses_when_loop_dir_symlink_escapes_loops_dir() {
        // Stage two tempdirs: `loops_dir` (the sandbox) and
        // `outside`. Make `loops_dir/escape-loop` a symlink pointing at
        // `outside`. delete_loop must refuse to rm-rf the symlink target.
        let loops = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let outside_canary = outside.path().join("DO_NOT_DELETE");
        std::fs::write(&outside_canary, b"keep").unwrap();

        // Symlink claim: loops/escape-loop -> outside
        let escape_link = loops.path().join("escape-loop");
        std::os::unix::fs::symlink(outside.path(), &escape_link).unwrap();
        // Minimal README so up-flow doesn't fail before we get to delete.
        std::fs::write(
            loops.path().join("README.md"),
            "### escape — label\n\n```\nx\n```\n",
        )
        .unwrap();

        let cfg_path = loops.path().join("workbench.toml");
        let cfg_str = format!(
            r#"
schema_version = 1

[workbench]
claude_loops_dir = "{}"
terminal = "konsole"
central_group = "1"

[[loops]]
name = "escape"
dir = "escape-loop"
readme_anchor = "escape--label"
group_key = "2"
schedule = "none"
dispatch = "resume_prompt"
"#,
            loops.path().display()
        );
        std::fs::write(&cfg_path, &cfg_str).unwrap();
        let mut cfg = crate::config::parse(&cfg_str).unwrap();
        let plugin = Arc::new(MockTerminalPlugin::new());
        let mut state = GlobalState::default();

        let summary =
            delete_loop(&mut cfg, &cfg_path, plugin, &mut state, "escape")
                .await
                .unwrap();
        // The symlink points outside the sandbox; the rm-rf step must be skipped.
        assert!(
            summary.dir_removed.is_none(),
            "delete_loop must refuse to rm-rf a symlink that points outside claude_loops_dir; \
             got dir_removed = {:?}",
            summary.dir_removed
        );
        // The canary file outside the sandbox must still exist.
        assert!(outside_canary.exists(), "canary file outside sandbox was deleted!");
    }

    #[tokio::test]
    async fn delete_loop_unknown_name_errors() {
        let dir = tempfile::tempdir().unwrap();
        write_min_loops_dir(
            dir.path(),
            &[("alpha — label", "alpha-dir")],
        );
        let cfg_path = dir.path().join("workbench.toml");
        let cfg_str = format!(
            r#"
schema_version = 1

[workbench]
claude_loops_dir = "{}"
terminal = "konsole"
central_group = "1"

[[loops]]
name = "alpha"
dir = "alpha-dir"
readme_anchor = "alpha--label"
group_key = "2"
schedule = "none"
dispatch = "resume_prompt"
"#,
            dir.path().display()
        );
        std::fs::write(&cfg_path, &cfg_str).unwrap();
        let mut cfg = crate::config::parse(&cfg_str).unwrap();
        let plugin = Arc::new(MockTerminalPlugin::new());
        let mut state = GlobalState::default();
        let err = delete_loop(&mut cfg, &cfg_path, plugin, &mut state, "ghost")
            .await
            .unwrap_err();
        assert!(err.to_string().contains("unknown loop"));
        // alpha untouched.
        assert!(dir.path().join("alpha-dir").exists());
        assert_eq!(cfg.loops.len(), 1);
    }

    #[tokio::test]
    async fn health_check_marks_healthy_panes_running() {
        let dir = tempfile::tempdir().unwrap();
        write_min_loops_dir(
            dir.path(),
            &[
                ("alpha — label", "alpha-dir"),
                ("beta — label", "beta-dir"),
                ("gamma — label", "gamma-dir"),
            ],
        );
        let cfg = cfg_for(dir.path());
        let plugin = Arc::new(MockTerminalPlugin::new());
        let mut state = GlobalState::default();
        up(&cfg, plugin.clone(), &mut state).await.unwrap();

        // After up, every pane should be running.
        for name in ["alpha", "beta", "gamma"] {
            assert!(matches!(
                state.loops.get(name).unwrap().pane_health,
                PaneHealth::Running
            ));
        }

        health_check_and_recover(&cfg, plugin.clone(), &mut state)
            .await
            .unwrap();
        for name in ["alpha", "beta", "gamma"] {
            assert!(matches!(
                state.loops.get(name).unwrap().pane_health,
                PaneHealth::Running
            ));
        }
    }

    #[tokio::test]
    async fn health_check_recreates_dead_pane_in_one_pass() {
        let dir = tempfile::tempdir().unwrap();
        write_min_loops_dir(
            dir.path(),
            &[
                ("alpha — label", "alpha-dir"),
                ("beta — label", "beta-dir"),
                ("gamma — label", "gamma-dir"),
            ],
        );
        let cfg = cfg_for(dir.path());
        let plugin = Arc::new(MockTerminalPlugin::new());
        let mut state = GlobalState::default();
        up(&cfg, plugin.clone(), &mut state).await.unwrap();

        let alpha_pane_before = state
            .loops
            .get("alpha")
            .unwrap()
            .pane_id
            .clone()
            .unwrap();
        let pane = crate::terminal::PaneId(alpha_pane_before.clone());
        plugin.kill(&pane); // simulate user closing the pane

        health_check_and_recover(&cfg, plugin.clone(), &mut state)
            .await
            .unwrap();

        let alpha = state.loops.get("alpha").unwrap();
        // Pane was recreated in the same pass → Running, with a NEW pane_id.
        assert!(matches!(alpha.pane_health, PaneHealth::Running));
        assert!(alpha.pane_id.is_some());
        assert_ne!(alpha.pane_id.as_deref(), Some(alpha_pane_before.as_str()));

        // Beta and gamma untouched — still on their original panes.
        assert!(matches!(
            state.loops.get("beta").unwrap().pane_health,
            PaneHealth::Running
        ));
    }

    #[tokio::test]
    async fn health_check_marks_failed_recreate_when_workdir_missing() {
        let dir = tempfile::tempdir().unwrap();
        write_min_loops_dir(
            dir.path(),
            &[
                ("alpha — label", "alpha-dir"),
                ("beta — label", "beta-dir"),
                ("gamma — label", "gamma-dir"),
            ],
        );
        let cfg = cfg_for(dir.path());
        let plugin = Arc::new(MockTerminalPlugin::new());
        let mut state = GlobalState::default();
        up(&cfg, plugin.clone(), &mut state).await.unwrap();

        let pane = crate::terminal::PaneId(
            state.loops.get("alpha").unwrap().pane_id.clone().unwrap(),
        );
        plugin.kill(&pane);

        // Sabotage recovery: delete the workdir so ensure_pane_for_loop errors.
        std::fs::remove_dir_all(dir.path().join("alpha-dir")).unwrap();

        health_check_and_recover(&cfg, plugin.clone(), &mut state)
            .await
            .unwrap();

        match &state.loops.get("alpha").unwrap().pane_health {
            PaneHealth::FailedRecreate {
                attempts,
                last_error,
                ..
            } => {
                assert_eq!(*attempts, 1);
                assert!(last_error.contains("workdir does not exist"));
            }
            other => panic!("expected FailedRecreate, got {other:?}"),
        }

        // Restore workdir; next pass should recover.
        std::fs::create_dir_all(dir.path().join("alpha-dir")).unwrap();
        health_check_and_recover(&cfg, plugin.clone(), &mut state)
            .await
            .unwrap();
        assert!(matches!(
            state.loops.get("alpha").unwrap().pane_health,
            PaneHealth::Running
        ));
    }

    #[tokio::test]
    async fn failed_recreate_attempts_increment_until_success() {
        let dir = tempfile::tempdir().unwrap();
        write_min_loops_dir(
            dir.path(),
            &[
                ("alpha — label", "alpha-dir"),
                ("beta — label", "beta-dir"),
                ("gamma — label", "gamma-dir"),
            ],
        );
        let cfg = cfg_for(dir.path());
        let plugin = Arc::new(MockTerminalPlugin::new());
        let mut state = GlobalState::default();
        up(&cfg, plugin.clone(), &mut state).await.unwrap();

        let pane = crate::terminal::PaneId(
            state.loops.get("alpha").unwrap().pane_id.clone().unwrap(),
        );
        plugin.kill(&pane);
        std::fs::remove_dir_all(dir.path().join("alpha-dir")).unwrap();

        for expected in [1u32, 2, 3] {
            health_check_and_recover(&cfg, plugin.clone(), &mut state)
                .await
                .unwrap();
            match &state.loops.get("alpha").unwrap().pane_health {
                PaneHealth::FailedRecreate { attempts, .. } => {
                    assert_eq!(*attempts, expected);
                }
                other => panic!("attempt {expected}: expected FailedRecreate, got {other:?}"),
            }
        }
    }
}