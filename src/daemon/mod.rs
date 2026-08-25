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

//! Long-lived daemon: owns the TerminalPlugin, the loaded WorkbenchConfig,
//! and the persistent GlobalState. Listens on a Unix socket for JSON-RPC
//! requests from the CLI subcommands.
//!
//! Step 5 wires the plumbing. The actual `up` orchestration logic lives in
//! step 6 (`commands::up`); this module exposes hooks the test suite can
//! drive against the mock plugin.

pub mod ipc;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{anyhow, bail, Context, Result};
use tokio::net::UnixListener;
use tokio::sync::{Mutex, Notify};

use crate::config::WorkbenchConfig;
use crate::state::{self, GlobalState};
use crate::terminal::TerminalPlugin;

/// Daemon-side shared state. Wrapped in `Arc<Mutex<...>>` for the IPC
/// handler.
pub struct DaemonContext {
    pub config: WorkbenchConfig,
    /// Path to workbench.toml — kept so `reload` can re-read on demand.
    pub config_path: PathBuf,
    pub state: GlobalState,
    pub plugin: Arc<dyn TerminalPlugin>,
    pub state_path: PathBuf,
    /// Set when the daemon should exit gracefully.
    pub shutdown: Arc<Notify>,
}

impl DaemonContext {
    /// Persist current state to disk. Snapshots the plugin's group cache
    /// into `state.groups` first so the next daemon run can re-adopt
    /// previously-spawned containers.
    pub fn persist(&mut self) -> Result<()> {
        self.state.groups = self.plugin.snapshot_groups();
        state::save(&self.state_path, &self.state)
    }
}

/// Latest migration version. Bumped by changes that need to mutate
/// existing user workbench.toml files atomically at daemon startup.
const LATEST_MIGRATION_V: u32 = 3;

/// Apply one-shot migrations to the in-memory config and persist them
/// back to `config_path`. Migrations are gated by
/// `WorkbenchConfig.loopctl_migration_v` — running the daemon on an
/// already-migrated file is a no-op.
fn apply_migrations(config: &mut WorkbenchConfig, config_path: &Path) -> Result<()> {
    if config.loopctl_migration_v >= LATEST_MIGRATION_V {
        return Ok(());
    }
    // v1: drop per-loop `claude_args = Some(vec![])` written by the
    // older auto-discover path. They were intended as opt-outs for
    // newly-added loops, but landed on every load-repo discovery —
    // silently overriding the workbench-wide default. Hand-edited
    // empty arrays would still be re-applied by the user, since
    // running this migration only flips them ONCE.
    if config.loopctl_migration_v < 1 {
        let mut stripped = 0usize;
        for lc in config.loops.iter_mut() {
            if matches!(&lc.agent_args, Some(v) if v.is_empty()) {
                lc.agent_args = None;
                stripped += 1;
            }
        }
        if stripped > 0 {
            tracing::info!(
                stripped,
                "migration v1: cleared per-loop claude_args=[] overrides; loops now inherit the workbench-wide default"
            );
        }
        config.loopctl_migration_v = 1;
    }

    // v2: rename per-loop `slug = "..."` to `name = "..."` on disk.
    // The serde alias on LoopConfig.name means the in-memory struct
    // already holds the right value regardless of which on-disk key
    // it came from; toml::to_string_pretty below emits `name`. So
    // this migration is effectively just bumping the version number
    // — the file rewrite happens unconditionally at the end of this
    // function and produces the new key.
    if config.loopctl_migration_v < 2 {
        tracing::info!(
            "migration v2: per-loop key `slug` is now `name`; older configs are accepted via serde alias and rewritten on persist"
        );
        config.loopctl_migration_v = 2;
    }

    // v3: `claude_binary`/`claude_args` are now `agent_binary`/`agent_args`
    // (workbench + per-loop), part of multi-agent support (claude / codex /
    // opencode). Like v2, the serde aliases mean the in-memory struct is
    // already correct regardless of the on-disk key; the unconditional
    // rewrite below emits the new names. Nothing to mutate here.
    if config.loopctl_migration_v < 3 {
        tracing::info!(
            "migration v3: `claude_binary`/`claude_args` renamed to `agent_binary`/`agent_args`; older configs are accepted via serde alias and rewritten on persist"
        );
        config.loopctl_migration_v = 3;
    }

    // Persist the migrated config so subsequent runs skip the migration.
    let body = toml::to_string_pretty(config)?;
    let tmp = config_path.with_extension("toml.tmp");
    std::fs::write(&tmp, body).with_context(|| format!("write {tmp:?}"))?;
    std::fs::rename(&tmp, config_path)
        .with_context(|| format!("rename to {config_path:?}"))?;
    crate::config::snapshot_in_git(
        config_path,
        &format!("loopctl: migration v{}", config.loopctl_migration_v),
    );
    Ok(())
}

/// Run the daemon foreground. Returns when the IPC loop says shut down.
pub async fn run(
    mut config: WorkbenchConfig,
    config_path: PathBuf,
    plugin: Arc<dyn TerminalPlugin>,
    state_path: PathBuf,
    socket_path: PathBuf,
) -> Result<()> {
    apply_migrations(&mut config, &config_path)
        .context("apply workbench.toml migrations")?;
    plugin
        .supports_environment()
        .map_err(|e| anyhow!("terminal plugin refused: {e}"))?;

    let mut state = state::load(&state_path).context("load state.json")?;
    state.terminal = plugin.name().to_string();
    state.started_at = Some(chrono::Utc::now());

    // Migration: state files written before `desired_up` existed (schema
    // < 2) have no restore intent recorded. Backfill it from the transient
    // `pane_id` — a loop with a live pane in the last-persisted state was,
    // by definition, up — so the first restart after upgrading still
    // restores the loops the user had running.
    if state.schema_version < state::CURRENT_SCHEMA_VERSION {
        for ls in state.loops.values_mut() {
            if ls.pane_id.is_some() {
                ls.desired_up = true;
            }
        }
        state.schema_version = state::CURRENT_SCHEMA_VERSION;
    }

    // Re-adopt any konsoles we spawned in a previous run before we wire
    // up IPC. Plugins that don't manage groups (mock) no-op.
    plugin
        .rehydrate(&state.groups)
        .await
        .context("plugin rehydrate")?;

    // Re-link each loop's pane_id by tab title. rehydrate restored the
    // group container (the konsole window), but state.json doesn't
    // remember which session_id inside that konsole belonged to which
    // loop — so without this step every loop sits `Unknown` even
    // though its tab is right there. We match by tab_title because
    // that's what the orchestrator stamps on `ensure_pane`.
    for lc in &config.loops {
        if !lc.active {
            continue;
        }
        match plugin
            .find_pane_by_title(&lc.group_key, lc.effective_tab_title())
            .await
        {
            Ok(Some(pane_id)) => {
                let entry = state.loops.entry(lc.name.clone()).or_default();
                let was = entry.pane_id.clone();
                entry.pane_id = Some(pane_id.0.clone());
                entry.pane_health = crate::state::PaneHealth::Running;
                tracing::info!(
                    name = %lc.name,
                    group = %lc.group_key,
                    pane = %pane_id.0,
                    was = ?was,
                    "relinked pane after rehydrate"
                );
            }
            Ok(None) => {
                // No matching tab in the cached konsole. Could be a
                // never-spawned loop or one whose tab the user closed.
                // Either way, leave state alone — health-check (or up)
                // will sort it out.
            }
            Err(e) => {
                tracing::warn!(name = %lc.name, error = %e, "find_pane_by_title failed during relink");
            }
        }
    }

    // Restore loops the user had live before the daemon stopped (crash,
    // reboot, or binary upgrade). Runs after relink, so loops whose window
    // survived are already adopted and no-op here; only `desired_up` +
    // `active` loops without a live pane get respawned. Never-upped loops
    // and anything the user `down`ed/closed stay down.
    let restored =
        crate::orchestrator::restore_desired_up(&config, plugin.clone(), &mut state).await;
    if restored > 0 {
        tracing::info!(restored, "restored previously-live loops on startup");
    }

    let shutdown = Arc::new(Notify::new());
    let ctx = Arc::new(Mutex::new(DaemonContext {
        config,
        config_path,
        state,
        plugin,
        state_path: state_path.clone(),
        shutdown: shutdown.clone(),
    }));
    {
        let mut ctx = ctx.lock().await;
        ctx.persist().context("write initial state.json")?;
    }

    let listener = bind_socket(&socket_path).context("bind ipc socket")?;
    tracing::info!(socket = ?socket_path, "daemon listening");
    let serve_ctx = ctx.clone();
    let serve = tokio::spawn(async move { ipc::serve(listener, serve_ctx).await });
    let scheduler_ctx = ctx.clone();
    let scheduler_shutdown = shutdown.clone();
    let scheduler = tokio::spawn(async move {
        scheduler_loop(scheduler_ctx, scheduler_shutdown).await;
    });

    // Wait for shutdown notify or ctrl-c.
    tokio::select! {
        _ = shutdown.notified() => {
            tracing::info!("graceful shutdown requested");
        }
        r = tokio::signal::ctrl_c() => {
            r.ok();
            tracing::info!("ctrl-c received");
        }
    }

    scheduler.abort();
    serve.abort();
    let _ = std::fs::remove_file(&socket_path);
    let mut ctx = ctx.lock().await;
    ctx.persist().context("write final state.json")?;
    Ok(())
}

/// Bind the socket, removing any stale file from a prior run.
fn bind_socket(path: &Path) -> Result<UnixListener> {
    if path.exists() {
        // If an old daemon is still listening, this will reject the new bind below.
        let _ = std::fs::remove_file(path);
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("mkdir -p {parent:?}"))?;
    }
    let listener = UnixListener::bind(path)
        .with_context(|| format!("bind unix socket {path:?}"))?;
    // Tighten the socket to user-only access so another local user
    // can't speak the IPC protocol against this daemon. The socket
    // sits under $XDG_RUNTIME_DIR which is already 0700, but the
    // socket itself defaults to umask-derived perms; pin it to 0600
    // so the guarantee doesn't depend on umask.
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .with_context(|| format!("chmod 0600 {path:?}"))?;
    Ok(listener)
}

/// Wakes up every minute, fires any loop whose `next_tick` is in the past.
/// On startup, computes initial `next_tick` for any loop that doesn't have
/// one yet using `last_tick = None`, so first ticks are not "due now".
async fn scheduler_loop(ctx: Arc<Mutex<DaemonContext>>, shutdown: Arc<Notify>) {
    // Hydrate next_tick for new loops; flip inactive loops' state to
    // PaneHealth::Inactive so the TUI shows them correctly.
    {
        let mut g = ctx.lock().await;
        let now = chrono::Utc::now();
        let cfg = g.config.clone();
        for lc in &cfg.loops {
            let entry = g.state.loops.entry(lc.name.clone()).or_default();
            if !lc.active {
                entry.pane_id = None;
                entry.pane_health = crate::state::PaneHealth::Inactive { since: now };
                entry.next_tick = None;
                continue;
            }
            if entry.next_tick.is_none() {
                if let Ok(s) = lc.parsed_schedule() {
                    entry.next_tick =
                        crate::scheduler::next_fire(&s, now, entry.last_tick);
                }
            }
        }
        let _ = g.persist();
    }

    loop {
        let sleep = tokio::time::sleep(std::time::Duration::from_secs(60));
        tokio::pin!(sleep);
        tokio::select! {
            _ = &mut sleep => {}
            _ = shutdown.notified() => return,
        }
        // Health pass first — recreate before firing scheduled ticks so
        // a fresh tick doesn't try to paste into a dead pane.
        run_health_pass(&ctx).await;
        let due_names = collect_due_names(&ctx).await;
        for name in due_names {
            fire_tick(&ctx, &name).await;
        }
    }
}

async fn run_health_pass(ctx: &Arc<Mutex<DaemonContext>>) {
    let mut g = ctx.lock().await;
    let plugin = g.plugin.clone();
    let cfg = g.config.clone();
    let mut state = std::mem::take(&mut g.state);
    drop(g);
    let result = crate::orchestrator::health_check_and_recover(&cfg, plugin, &mut state).await;
    let mut g = ctx.lock().await;
    g.state = state;
    if let Err(e) = result {
        tracing::warn!(error = %e, "health pass returned error");
    }
    let _ = g.persist();
}

async fn collect_due_names(ctx: &Arc<Mutex<DaemonContext>>) -> Vec<String> {
    let g = ctx.lock().await;
    let now = chrono::Utc::now();
    g.config
        .loops
        .iter()
        .filter_map(|lc| {
            if !lc.active {
                return None;
            }
            let st = g.state.loops.get(&lc.name)?;
            let nt = st.next_tick?;
            if nt <= now {
                Some(lc.name.clone())
            } else {
                None
            }
        })
        .collect()
}

async fn fire_tick(ctx: &Arc<Mutex<DaemonContext>>, name: &str) {
    let mut g = ctx.lock().await;
    let plugin = g.plugin.clone();
    let cfg = g.config.clone();
    let mut state = std::mem::take(&mut g.state);
    drop(g);
    let report = crate::dispatch::tick_one(&cfg, plugin, &mut state, name).await;
    let mut g = ctx.lock().await;
    g.state = state;
    let _ = g.persist();
    match report {
        Ok(r) => tracing::info!(name, kind = r.kind, exit = ?r.exit, "scheduled tick fired"),
        Err(e) => tracing::warn!(name, error = %e, "scheduled tick failed"),
    }
}

/// Acquire a lock so only one daemon runs per user. Lock file lives next
/// to the socket. Returns the open file (caller must keep it alive for
/// the duration of the daemon).
pub fn acquire_lock(socket_path: &Path) -> Result<std::fs::File> {
    use std::os::unix::io::AsRawFd;
    let lock_path = socket_path.with_extension("lock");
    if let Some(parent) = lock_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let f = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&lock_path)
        .with_context(|| format!("open lockfile {lock_path:?}"))?;
    let fd = f.as_raw_fd();
    let rc = unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) };
    if rc != 0 {
        bail!("another loopctl daemon is already running (lock on {lock_path:?})");
    }
    Ok(f)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(body: &str) -> WorkbenchConfig {
        toml::from_str(body).unwrap()
    }

    #[test]
    fn migration_v1_strips_empty_claude_args() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("workbench.toml");
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
claude_args = []

[[loops]]
name = "beta"
dir = "beta"
readme_anchor = "beta"
group_key = "8"
schedule = "none"
dispatch = "resume_prompt"
claude_args = ["--allowed-tools", "Read"]
"#;
        std::fs::write(&p, body).unwrap();
        let mut cfg = parse(body);
        apply_migrations(&mut cfg, &p).unwrap();
        // Migration v1 stripped empty per-loop arg overrides; later
        // migrations just bump the version and rewrite on persist. Latest
        // is now 3 (claude_* → agent_* rename).
        assert_eq!(cfg.loopctl_migration_v, 3);
        assert!(cfg.loops[0].agent_args.is_none(), "alpha [] should be cleared");
        assert_eq!(
            cfg.loops[1].agent_args.as_deref(),
            Some(&["--allowed-tools".to_string(), "Read".to_string()][..]),
            "beta with real args must NOT be touched"
        );
    }

    #[test]
    fn migration_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("workbench.toml");
        let body = r#"
schema_version = 1
loopctl_migration_v = 3

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
agent_args = []
"#;
        std::fs::write(&p, body).unwrap();
        let mut cfg = parse(body);
        apply_migrations(&mut cfg, &p).unwrap();
        // Already at the latest version: hand-set [] is left alone.
        assert_eq!(cfg.loops[0].agent_args.as_deref(), Some(&[][..]));
        assert_eq!(cfg.loopctl_migration_v, 3);
    }

    #[test]
    fn migration_v3_rewrites_legacy_claude_fields_to_agent() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("workbench.toml");
        // Pre-v3 config using the old claude_binary/claude_args keys.
        let body = r#"
schema_version = 1
loopctl_migration_v = 2

[workbench]
claude_loops_dir = "~/x"
terminal = "konsole"
central_group = "1"
claude_binary = "claude"
claude_args = ["--dangerously-skip-permissions"]

[[loops]]
name = "alpha"
dir = "alpha"
readme_anchor = "alpha"
group_key = "7"
schedule = "none"
dispatch = "resume_prompt"
claude_args = ["--allowed-tools", "Read"]
"#;
        std::fs::write(&p, body).unwrap();
        let mut cfg = parse(body);
        apply_migrations(&mut cfg, &p).unwrap();
        assert_eq!(cfg.loopctl_migration_v, 3);
        // On-disk file must now use the new key names and none of the old.
        let on_disk = std::fs::read_to_string(&p).unwrap();
        assert!(on_disk.contains("agent_binary"), "expected agent_binary:\n{on_disk}");
        assert!(on_disk.contains("agent_args"), "expected agent_args:\n{on_disk}");
        assert!(
            !on_disk.contains("claude_binary") && !on_disk.contains("claude_args"),
            "legacy claude_* keys must be gone:\n{on_disk}"
        );
    }

    #[test]
    fn migration_v2_rewrites_legacy_slug_field_to_name() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("workbench.toml");
        // Old config: uses `slug` (parses via serde alias) and is at v1.
        let body = r#"
schema_version = 1
loopctl_migration_v = 1

[workbench]
claude_loops_dir = "~/x"
terminal = "konsole"
central_group = "1"

[[loops]]
slug = "alpha"
dir = "alpha"
readme_anchor = "alpha"
group_key = "7"
schedule = "none"
dispatch = "resume_prompt"
"#;
        std::fs::write(&p, body).unwrap();
        let mut cfg = parse(body);
        apply_migrations(&mut cfg, &p).unwrap();
        assert_eq!(cfg.loopctl_migration_v, 3);
        assert_eq!(cfg.loops[0].name, "alpha");
        // The persisted file now uses `name = ...` not `slug = ...`.
        let on_disk = std::fs::read_to_string(&p).unwrap();
        assert!(on_disk.contains("name = \"alpha\""), "expected name on disk:\n{on_disk}");
        assert!(!on_disk.contains("slug = \"alpha\""));
    }
}