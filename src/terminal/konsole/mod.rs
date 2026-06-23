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

//! Konsole + KWin (KDE Plasma X11) implementation of [`TerminalPlugin`].
//!
//! Group key = X11 virtual desktop number (as a string). One Konsole window
//! per group; tabs within are matched by title.
//!
//! Each ensured Konsole's bus name (`org.kde.konsole-<pid>`) is cached in
//! memory keyed by `group_key`. The cache survives the daemon process
//! (state.json), but not within the same process — that's why we re-probe
//! every call rather than trusting stale handles.

use std::collections::HashSet;
use std::process::Stdio;
use std::time::Duration;

use anyhow::{anyhow, bail, Context};
use async_trait::async_trait;
use parking_lot::RwLock;
use tokio::process::Command;
use tokio::time::{sleep, Instant};
use zbus::fdo::DBusProxy;
use zbus::Connection;

use super::{
    normalize_paste, PaneId, PaneSpec, PluginResult, TerminalPlugin,
    UnsupportedReason,
};

pub mod dbus;

use dbus::{KonsoleSessionProxy, KonsoleWindowProxy};

/// Format: `konsole:<bus-pid>:<session-id>`. Stable across paste/run/focus.
fn make_pane_id(bus_pid: i32, session_id: i32) -> PaneId {
    PaneId::new(format!("konsole:{bus_pid}:{session_id}"))
}

fn parse_pane_id(id: &PaneId) -> anyhow::Result<(i32, i32)> {
    let s = id.as_str();
    let rest = s
        .strip_prefix("konsole:")
        .ok_or_else(|| anyhow!("not a konsole pane id: {s}"))?;
    let (pid, sid) = rest
        .split_once(':')
        .ok_or_else(|| anyhow!("malformed konsole pane id: {s}"))?;
    Ok((pid.parse()?, sid.parse()?))
}

fn bus_name_from_pid(pid: i32) -> String {
    format!("org.kde.konsole-{pid}")
}

#[derive(Debug, Default)]
struct Cache {
    /// group_key → (pid, bus_name, group_uuid) for the Konsole assigned
    /// to that group. The uuid is written into the first session's
    /// environment as `LOOPCTL_GROUP_UUID=<uuid>` so we can re-discover
    /// the konsole after a daemon restart.
    by_group: std::collections::HashMap<String, (i32, String, String)>,
}

/// Marker env var written into a managed konsole's first session.
/// Format: `LOOPCTL_GROUP_UUID=<short-uuid>`.
const LOOPCTL_GROUP_UUID_ENV: &str = "LOOPCTL_GROUP_UUID";
const LOOPCTL_GROUP_KEY_ENV: &str = "LOOPCTL_GROUP_KEY";

/// Title we stamp on the konsole's auto-spawned first session so
/// `ensure_pane` can recycle it for the first loop's tab instead of
/// leaving a "Built-in" leftover next to a real tab.
const PARKED_SESSION_TITLE: &str = "__loopctl_idle";

fn short_uuid() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    // Mix in process pid for in-flight uniqueness.
    let pid = std::process::id();
    format!("{:x}-{:x}", pid, nanos & 0xFFFF_FFFF)
}

/// Konsole plugin instance.
pub struct KonsolePlugin {
    profile: Option<String>,
    cache: RwLock<Cache>,
    /// Milliseconds to wait between pasting prompt body and sending CR.
    /// Configurable via `WorkbenchSettings.paste_submit_delay_ms`.
    paste_submit_delay_ms: u64,
}

impl KonsolePlugin {
    /// `profile` is an optional Konsole profile name (matches
    /// `[terminal.konsole].profile` in workbench.toml). When `Some`,
    /// every new tab is opened with that profile.
    pub fn new(profile: Option<String>) -> Self {
        Self {
            profile,
            cache: RwLock::new(Cache::default()),
            paste_submit_delay_ms: 700,
        }
    }

    /// Override the paste→submit delay (used by daemon startup with the
    /// value from workbench.toml). Builder-style.
    pub fn with_paste_submit_delay_ms(mut self, ms: u64) -> Self {
        self.paste_submit_delay_ms = ms;
        self
    }

    async fn session_bus() -> PluginResult<Connection> {
        Connection::session()
            .await
            .context("connect to D-Bus session bus")
    }

    async fn list_konsole_bus_names(conn: &Connection) -> PluginResult<HashSet<String>> {
        let dbus = DBusProxy::new(conn).await?;
        let names = dbus.list_names().await?;
        Ok(names
            .into_iter()
            .filter_map(|n| {
                let s = n.to_string();
                s.starts_with("org.kde.konsole-").then_some(s)
            })
            .collect())
    }

    fn pid_from_bus(bus_name: &str) -> Option<i32> {
        bus_name
            .strip_prefix("org.kde.konsole-")
            .and_then(|s| s.parse().ok())
    }

    fn pid_alive(pid: i32) -> bool {
        use nix::sys::signal;
        use nix::unistd::Pid;
        signal::kill(Pid::from_raw(pid), None).is_ok()
    }

    /// Returns the cached (pid, bus_name, uuid) for `group_key` if it's
    /// still alive AND still on the bus. Otherwise drops the cache entry
    /// and returns None.
    async fn cached_alive(
        &self,
        conn: &Connection,
        group_key: &str,
    ) -> PluginResult<Option<(i32, String, String)>> {
        let cached = self.cache.read().by_group.get(group_key).cloned();
        let Some((pid, bus, uuid)) = cached else {
            return Ok(None);
        };
        if !Self::pid_alive(pid) {
            self.cache.write().by_group.remove(group_key);
            return Ok(None);
        }
        let names = Self::list_konsole_bus_names(conn).await?;
        if !names.contains(&bus) {
            self.cache.write().by_group.remove(group_key);
            return Ok(None);
        }
        Ok(Some((pid, bus, uuid)))
    }

    /// Walk every konsole on the bus and return the first one whose
    /// first session carries `LOOPCTL_GROUP_UUID=<uuid>`. Used for
    /// re-discovery after a daemon restart.
    async fn find_konsole_by_uuid(
        conn: &Connection,
        uuid: &str,
    ) -> PluginResult<Option<(i32, String)>> {
        for bus in Self::list_konsole_bus_names(conn).await? {
            let pid = match Self::pid_from_bus(&bus) {
                Some(p) => p,
                None => continue,
            };
            if !Self::pid_alive(pid) {
                continue;
            }
            // Look at every session in this konsole; the marker only
            // lives on the first session we created, but other sessions
            // in the same konsole would never carry it, so this is fine.
            let win = Self::window_proxy(conn, &bus).await?;
            let raw_ids = match win.session_list().await {
                Ok(v) => v,
                Err(_) => continue,
            };
            for raw in raw_ids {
                let sid: i32 = match raw.parse() {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                let session = match Self::session_proxy(conn, &bus, sid).await {
                    Ok(s) => s,
                    Err(_) => continue,
                };
                let env = session.environment().await.unwrap_or_default();
                let marker = format!("{LOOPCTL_GROUP_UUID_ENV}={uuid}");
                if env.iter().any(|e| e == &marker) {
                    return Ok(Some((pid, bus.clone())));
                }
            }
        }
        Ok(None)
    }

    /// Tag the first session of a freshly-spawned konsole with our
    /// markers so future runs can adopt it without spawning a duplicate.
    async fn tag_first_session(
        conn: &Connection,
        bus: &str,
        group_key: &str,
        uuid: &str,
    ) -> PluginResult<()> {
        let win = Self::window_proxy(conn, bus).await?;
        let sessions = win.session_list().await?;
        let Some(raw) = sessions.into_iter().next() else {
            return Ok(());
        };
        let sid: i32 = raw
            .parse()
            .with_context(|| format!("session id `{raw}` not int"))?;
        let session = Self::session_proxy(conn, bus, sid).await?;
        let mut env = session.environment().await.unwrap_or_default();
        env.retain(|e| {
            !e.starts_with(&format!("{LOOPCTL_GROUP_UUID_ENV}="))
                && !e.starts_with(&format!("{LOOPCTL_GROUP_KEY_ENV}="))
        });
        env.push(format!("{LOOPCTL_GROUP_UUID_ENV}={uuid}"));
        env.push(format!("{LOOPCTL_GROUP_KEY_ENV}={group_key}"));
        session.set_environment(env).await?;
        // Park the auto-spawned first tab under a sentinel title so
        // `ensure_pane` can recycle it for the first loop's tab.
        let _ = session.set_title(0, PARKED_SESSION_TITLE).await;
        Ok(())
    }


    async fn spawn_konsole(workdir: &std::path::Path) -> PluginResult<()> {
        Command::new("konsole")
            .arg("--separate")
            .arg("--workdir")
            .arg(workdir)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .context("spawn konsole")?;
        Ok(())
    }

    async fn wait_for_new_bus_name(
        conn: &Connection,
        before: &HashSet<String>,
        timeout_secs: u64,
    ) -> PluginResult<String> {
        let deadline = Instant::now() + Duration::from_secs(timeout_secs);
        loop {
            let now = Self::list_konsole_bus_names(conn).await?;
            if let Some(new) = now.difference(before).next() {
                return Ok(new.clone());
            }
            if Instant::now() >= deadline {
                bail!("timed out waiting for new org.kde.konsole-* bus name");
            }
            sleep(Duration::from_millis(100)).await;
        }
    }

    async fn window_proxy<'a>(
        conn: &'a Connection,
        bus: &str,
    ) -> PluginResult<KonsoleWindowProxy<'a>> {
        Ok(KonsoleWindowProxy::builder(conn)
            .destination(bus.to_string())?
            .path("/Windows/1")?
            .build()
            .await?)
    }

    async fn session_proxy<'a>(
        conn: &'a Connection,
        bus: &str,
        session_id: i32,
    ) -> PluginResult<KonsoleSessionProxy<'a>> {
        Ok(KonsoleSessionProxy::builder(conn)
            .destination(bus.to_string())?
            .path(format!("/Sessions/{session_id}"))?
            .build()
            .await?)
    }

    /// Look for an existing tab whose role-0 title matches `title`.
    /// Returns `(session_id, true)` if found.
    async fn find_tab_by_title(
        conn: &Connection,
        bus: &str,
        title: &str,
    ) -> PluginResult<Option<i32>> {
        let win = Self::window_proxy(conn, bus).await?;
        let raw_ids = win.session_list().await?;
        for raw in raw_ids {
            let sid: i32 = match raw.parse() {
                Ok(v) => v,
                Err(_) => continue,
            };
            let session = Self::session_proxy(conn, bus, sid).await?;
            // Role 0 = local; that's what we set.
            if session.title(0).await.unwrap_or_default() == title {
                return Ok(Some(sid));
            }
        }
        Ok(None)
    }

    /// Best-effort wait for the user-supplied initial command to take over
    /// the foreground in this session. Falls back to a fixed sleep if the
    /// foreground PID never changes (some commands are too fast).
    async fn wait_for_foreground_change(
        session: &KonsoleSessionProxy<'_>,
        timeout: Duration,
    ) -> PluginResult<()> {
        let shell_pid = session.foreground_process_id().await.unwrap_or(-1);
        let deadline = Instant::now() + timeout;
        loop {
            if Instant::now() >= deadline {
                break;
            }
            let cur = session.foreground_process_id().await.unwrap_or(shell_pid);
            if cur != shell_pid && cur > 0 {
                return Ok(());
            }
            sleep(Duration::from_millis(100)).await;
        }
        // Fallback — give the command another beat.
        sleep(Duration::from_millis(500)).await;
        Ok(())
    }

    /// Move a window owned by `pid` to virtual desktop `desktop` (1-indexed).
    /// Implemented via KWin Scripting because no D-Bus method on /KWin
    /// takes a (pid, desktop) pair directly.
    async fn send_window_to_desktop(pid: i32, desktop: i32) -> PluginResult<()> {
        // Plasma 5 exposes `workspace.clientList()` + `sendClientToDesktop`.
        // Plasma 6 renames to `workspace.windowList()` and uses
        // `client.desktops = [workspace.desktops[i]]`. The snippet below
        // works on either.
        // Plasma 5.27 (verified on this host) only exposes the legacy
        // `client.desktop` setter — `sendClientToDesktop` and the
        // `client.desktops = [...]` array form are both absent.
        // Plasma 6 keeps `client.desktop` as a fallback.
        let script = format!(
            r#"
            (function() {{
                var target_pid = {pid};
                var target_desktop = {desktop};
                var list = (typeof workspace.windowList === "function")
                    ? workspace.windowList()
                    : workspace.clientList();
                for (var i = 0; i < list.length; i++) {{
                    var c = list[i];
                    if (c.pid === target_pid) {{
                        c.desktop = target_desktop;
                    }}
                }}
            }})();
            "#
        );

        let mut path = std::env::temp_dir();
        path.push(format!("loopctl-kwin-{pid}-{desktop}.js"));
        std::fs::write(&path, script).context("write kwin script")?;

        let conn = Self::session_bus().await?;
        let scripting = zbus::Proxy::new(
            &conn,
            "org.kde.KWin",
            "/Scripting",
            "org.kde.kwin.Scripting",
        )
        .await?;
        let plugin_name = format!("loopctl-{pid}-{desktop}");
        let path_str = path.to_string_lossy().to_string();
        let id: i32 = scripting
            .call("loadScript", &(path_str.clone(), plugin_name.clone()))
            .await
            .context("KWin loadScript")?;
        let _: () = scripting
            .call("start", &())
            .await
            .context("KWin Scripting.start")?;
        // Give KWin a beat to run the snippet before we unload it.
        sleep(Duration::from_millis(150)).await;
        let _: bool = scripting
            .call("unloadScript", &(plugin_name,))
            .await
            .unwrap_or(false);
        let _ = std::fs::remove_file(&path);
        let _ = id;
        Ok(())
    }
}

#[async_trait]
impl TerminalPlugin for KonsolePlugin {
    fn name(&self) -> &'static str {
        "konsole"
    }

    fn supports_environment(&self) -> Result<(), UnsupportedReason> {
        if which::which("konsole").is_err() {
            return Err(UnsupportedReason::MissingDependency {
                backend: self.name(),
                requirement: "the `konsole` binary on PATH".to_string(),
            });
        }
        match std::env::var("XDG_SESSION_TYPE").as_deref() {
            Ok("x11") | Ok("") | Err(_) => Ok(()),
            Ok(other) => Err(UnsupportedReason::WrongPlatform {
                backend: self.name(),
                detail: format!(
                    "v1 supports X11 only; XDG_SESSION_TYPE={other}. Wayland support is planned."
                ),
            }),
        }
    }

    async fn ensure_group(&self, group_key: &str) -> PluginResult<()> {
        let conn = Self::session_bus().await?;

        if self.cached_alive(&conn, group_key).await?.is_some() {
            return Ok(());
        }

        // Always spawn a fresh konsole. Adoption of *unmanaged* konsoles
        // is forbidden — adding tabs to someone else's working window is
        // intrusive. Adoption of OUR OWN previously-spawned konsoles
        // happens via `rehydrate()` at daemon startup, which reads the
        // LOOPCTL_GROUP_UUID env we wrote into the first session.
        let workdir = std::env::var("HOME").context("HOME must be set to spawn konsole")?;
        let before = Self::list_konsole_bus_names(&conn).await?;
        Self::spawn_konsole(std::path::Path::new(&workdir)).await?;
        let bus = Self::wait_for_new_bus_name(&conn, &before, 5).await?;
        let pid = Self::pid_from_bus(&bus)
            .ok_or_else(|| anyhow!("malformed konsole bus name: {bus}"))?;

        // Tag the first session BEFORE doing anything else, so a daemon
        // crash mid-spawn still leaves the konsole adoptable.
        let uuid = short_uuid();
        if let Err(e) = Self::tag_first_session(&conn, &bus, group_key, &uuid).await {
            tracing::warn!(error = %e, "tagging first session failed; konsole won't be re-adoptable");
        }

        // Best-effort: move the window onto the configured virtual desktop.
        // Group key is the X11 virtual desktop number when it parses as int.
        if let Ok(desktop) = group_key.parse::<i32>() {
            if let Err(e) = Self::send_window_to_desktop(pid, desktop).await {
                tracing::warn!(
                    pid,
                    desktop,
                    error = %e,
                    "could not move new konsole to virtual desktop; user may need to drag it manually"
                );
            }
        }

        self.cache
            .write()
            .by_group
            .insert(group_key.to_string(), (pid, bus, uuid));
        Ok(())
    }


    async fn ensure_pane(&self, spec: &PaneSpec) -> PluginResult<PaneId> {
        let conn = Self::session_bus().await?;
        self.ensure_group(&spec.group_key).await?;
        let (pid, bus, _uuid) = self
            .cache
            .read()
            .by_group
            .get(&spec.group_key)
            .cloned()
            .ok_or_else(|| anyhow!("ensure_group did not populate cache for {}", spec.group_key))?;

        // Idempotent: if a tab with this title already exists, reuse it
        // and skip running the initial command.
        if let Some(existing) = Self::find_tab_by_title(&conn, &bus, &spec.title).await? {
            return Ok(make_pane_id(pid, existing));
        }

        // If the konsole's auto-spawned first session is still parked
        // under the sentinel title, recycle it instead of creating a
        // brand-new tab. This collapses what would otherwise be two
        // visible tabs (Built-in + our pane) into one.
        let workdir_str = spec.workdir.to_string_lossy().to_string();
        let win = Self::window_proxy(&conn, &bus).await?;
        let mut recycled = false;
        let new_sid = if let Some(parked) =
            Self::find_tab_by_title(&conn, &bus, PARKED_SESSION_TITLE).await?
        {
            // `cd` the parked shell into the workdir. Its title gets
            // overwritten below.
            let session = Self::session_proxy(&conn, &bus, parked).await?;
            session
                .run_command(&format!("cd {}", shell_escape(&workdir_str)))
                .await
                .context("recycled session: cd")?;
            recycled = true;
            parked
        } else {
            match &self.profile {
                Some(p) => win
                    .new_session_with_profile_dir(p, &workdir_str)
                    .await
                    .context("Konsole.newSession(profile, dir)")?,
                None => {
                    // newSession() ignores workdir; we cd manually below.
                    win.new_session_default()
                        .await
                        .context("Konsole.newSession()")?
                }
            }
        };

        let session = Self::session_proxy(&conn, &bus, new_sid).await?;
        // Title BEFORE any runCommand — it's our stable identity.
        session
            .set_title(0, &spec.title)
            .await
            .context("set tab title")?;
        session
            .set_title(1, &spec.title)
            .await
            .context("set remote title")?;

        // If we created a fresh tab in the no-profile branch, cd into
        // the workdir. Recycled sessions already cd'd above.
        if !recycled && self.profile.is_none() {
            let cd = format!("cd {}", shell_escape(&workdir_str));
            session.run_command(&cd).await.context("run cd")?;
        }

        if let Some(cmd) = &spec.initial_command {
            session
                .run_command(cmd)
                .await
                .context("run initial_command")?;
            Self::wait_for_foreground_change(&session, Duration::from_secs(3)).await?;
        }

        Ok(make_pane_id(pid, new_sid))
    }

    async fn paste(&self, pane: &PaneId, text: &str) -> PluginResult<()> {
        // Splitting body + submit into two sendText calls is intentional:
        // claude (a TUI) treats CR (\r) as "submit prompt" and LF (\n) as
        // "literal newline within the prompt". If we ended the body with
        // \r the user couldn't review before submission; if we ended it
        // with \n nothing ever submits. Send the body with \n separators
        // so embedded paragraph breaks render correctly in the prompt
        // composer, then a separate \r to fire the submit.
        let (pid, sid) = parse_pane_id(pane)?;
        let conn = Self::session_bus().await?;
        let bus = bus_name_from_pid(pid);
        let session = Self::session_proxy(&conn, &bus, sid).await?;

        let body = normalize_paste(text);
        let body = body.trim_end_matches('\n');
        if !body.is_empty() {
            session
                .send_text(body)
                .await
                .context("sendText body")?;
        }
        // Submit. claude (Ink) treats CR as "submit prompt" only after
        // it has enabled raw mode on the pty — before that, the pty's
        // default `icrnl` would translate \r to \n, which Ink then
        // displays as a literal newline in the composer. So we sleep
        // long enough for claude to finish startup + raw mode, then
        // send \r. Configurable via paste_submit_delay_ms.
        sleep(Duration::from_millis(self.paste_submit_delay_ms)).await;
        session
            .send_text("\r")
            .await
            .context("sendText submit (CR)")?;
        Ok(())
    }

    async fn run(&self, pane: &PaneId, command: &str) -> PluginResult<()> {
        let (pid, sid) = parse_pane_id(pane)?;
        let conn = Self::session_bus().await?;
        let bus = bus_name_from_pid(pid);
        let session = Self::session_proxy(&conn, &bus, sid).await?;
        session.run_command(command).await.context("runCommand")?;
        Ok(())
    }

    async fn focus(&self, pane: &PaneId) -> PluginResult<()> {
        let (pid, sid) = parse_pane_id(pane)?;
        let conn = Self::session_bus().await?;
        let bus = bus_name_from_pid(pid);
        let win = Self::window_proxy(&conn, &bus).await?;
        win.set_current_session(sid)
            .await
            .context("setCurrentSession")?;
        // Raising the window itself is a KWin job — deferred. Today
        // setCurrentSession is enough when the user is already on the
        // right virtual desktop.
        Ok(())
    }

    async fn close(&self, pane: &PaneId) -> PluginResult<()> {
        // Konsole 5.27 doesn't expose a tab-close method on
        // `org.kde.konsole.Session` (it's gone in some 5.x builds).
        // Killing the session's shell PID with SIGHUP causes Konsole
        // to drop the tab; this also takes claude (running inside
        // that pty) down with it. We additionally fall back to the
        // (non-existent in 5.27) `Session.close` D-Bus method so
        // future Konsole versions that add it back also work.
        let (pid, sid) = parse_pane_id(pane)?;
        let conn = Self::session_bus().await?;
        let bus = bus_name_from_pid(pid);
        let session = Self::session_proxy(&conn, &bus, sid).await?;
        let shell_pid = session.process_id().await.unwrap_or(-1);
        if shell_pid > 0 {
            use nix::sys::signal;
            use nix::unistd::Pid;
            let _ = signal::kill(Pid::from_raw(shell_pid), signal::Signal::SIGHUP);
        }
        // Best-effort: try the (often-missing) close method too. The
        // proxy still declares it for back-compat; ignore errors.
        let _ = session.close().await;
        Ok(())
    }

    async fn pane_alive(&self, pane: &PaneId) -> PluginResult<bool> {
        let (pid, sid) = parse_pane_id(pane)?;
        if !Self::pid_alive(pid) {
            return Ok(false);
        }
        let conn = Self::session_bus().await?;
        let bus = bus_name_from_pid(pid);
        let names = Self::list_konsole_bus_names(&conn).await?;
        if !names.contains(&bus) {
            return Ok(false);
        }
        // Walk the window's session list; if our id isn't there, the
        // user closed just this tab.
        let win = Self::window_proxy(&conn, &bus).await?;
        let raw_ids = match win.session_list().await {
            Ok(v) => v,
            // The konsole exists but its window introspection is
            // momentarily unavailable — treat as a transient error,
            // not a definitive "dead", so we don't recreate on a flake.
            Err(e) => return Err(e.into()),
        };
        let want = sid.to_string();
        Ok(raw_ids.iter().any(|s| s == &want))
    }

    fn snapshot_groups(
        &self,
    ) -> std::collections::BTreeMap<String, crate::state::GroupState> {
        let mut out = std::collections::BTreeMap::new();
        for (group, (pid, bus, uuid)) in &self.cache.read().by_group {
            out.insert(
                group.clone(),
                crate::state::GroupState {
                    pid: *pid,
                    bus_name: bus.clone(),
                    uuid: Some(uuid.clone()),
                },
            );
        }
        out
    }

    fn forget_group(&self, group_key: &str) {
        self.cache.write().by_group.remove(group_key);
    }

    async fn move_group_to(&self, old_key: &str, new_key: &str) -> PluginResult<()> {
        let new_desktop: i32 = new_key
            .parse()
            .with_context(|| format!("group_key `{new_key}` is not an integer (X11 desktop)"))?;
        let pid = self
            .cache
            .read()
            .by_group
            .get(old_key)
            .map(|(p, _, _)| *p)
            .ok_or_else(|| anyhow!("no cached konsole for group `{old_key}`"))?;
        Self::send_window_to_desktop(pid, new_desktop).await?;
        Ok(())
    }

    fn rekey_group(&self, old_key: &str, new_key: &str) {
        let mut c = self.cache.write();
        if let Some(v) = c.by_group.remove(old_key) {
            c.by_group.insert(new_key.to_string(), v);
        }
    }

    async fn desktop_count(&self) -> PluginResult<u32> {
        let conn = Self::session_bus().await?;
        let mgr = zbus::Proxy::new(
            &conn,
            "org.kde.KWin",
            "/VirtualDesktopManager",
            "org.freedesktop.DBus.Properties",
        )
        .await?;
        let v: zbus::zvariant::OwnedValue = mgr
            .call("Get", &("org.kde.KWin.VirtualDesktopManager", "count"))
            .await?;
        let n: u32 = v.try_into().unwrap_or(0);
        Ok(n)
    }

    async fn empty_desktops(&self, desktop_count: u32) -> PluginResult<Vec<u32>> {
        // Walk every X11 client KWin knows about, count windows per
        // desktop, return desktops with zero windows in ascending order.
        // (We treat "all-desktop" windows like sticky ones as not
        // occupying any single desktop.)
        let counts = Self::desktop_window_counts().await?;
        let mut empty: Vec<u32> = (1..=desktop_count)
            .filter(|d| !counts.contains_key(d) || counts[d] == 0)
            .collect();
        empty.sort();
        Ok(empty)
    }

    async fn rehydrate(
        &self,
        previous: &std::collections::BTreeMap<String, crate::state::GroupState>,
    ) -> PluginResult<()> {
        let conn = Self::session_bus().await?;
        for (group, gs) in previous {
            let Some(uuid) = gs.uuid.as_ref() else {
                continue;
            };
            if let Some((pid, bus)) = Self::find_konsole_by_uuid(&conn, uuid).await? {
                tracing::info!(
                    group = %group,
                    pid,
                    bus = %bus,
                    "rehydrated konsole for group"
                );
                self.cache
                    .write()
                    .by_group
                    .insert(group.clone(), (pid, bus, uuid.clone()));
            }
        }
        Ok(())
    }

    async fn find_pane_by_title(
        &self,
        group_key: &str,
        title: &str,
    ) -> PluginResult<Option<PaneId>> {
        // Use the cached konsole for this group; if there isn't one,
        // we have nothing to look in. We deliberately don't spawn here
        // — that's `ensure_group`'s job.
        let cached = self.cache.read().by_group.get(group_key).cloned();
        let Some((pid, bus, _uuid)) = cached else {
            return Ok(None);
        };
        let conn = Self::session_bus().await?;
        Ok(Self::find_tab_by_title(&conn, &bus, title)
            .await?
            .map(|sid| make_pane_id(pid, sid)))
    }
}

impl KonsolePlugin {
    /// Returns desktop_number → window_count via a one-shot KWin script.
    /// Implementation detail: the JS prints lines in `LOOPCTL_DESK_COUNT
    /// <num> <count>` form to KWin's journalctl-visible log, then we
    /// scrape them. Less elegant than a return value, but KWin Scripting
    /// has no synchronous return path.
    async fn desktop_window_counts() -> PluginResult<std::collections::HashMap<u32, u32>> {
        use std::collections::HashMap;
        let marker = format!("LOOPCTL_DESK_COUNT_{}", std::process::id());
        let script = format!(
            r#"
            (function() {{
                var counts = {{}};
                var list = (typeof workspace.windowList === "function")
                    ? workspace.windowList()
                    : workspace.clientList();
                for (var i = 0; i < list.length; i++) {{
                    var c = list[i];
                    // Skip windows that belong to all desktops (sticky)
                    // or aren't on a specific desktop.
                    if (typeof c.onAllDesktops !== "undefined" && c.onAllDesktops) continue;
                    if (typeof c.specialWindow !== "undefined" && c.specialWindow) continue;
                    var d = (typeof c.desktop === "number") ? c.desktop : -1;
                    if (d <= 0) continue;
                    counts[d] = (counts[d] || 0) + 1;
                }}
                for (var k in counts) {{
                    print("{marker} " + k + " " + counts[k]);
                }}
            }})();
            "#
        );
        let mut path = std::env::temp_dir();
        path.push(format!("loopctl-empty-desk-{}.js", std::process::id()));
        std::fs::write(&path, script).context("write kwin probe script")?;

        // Stamp a sentinel into the journal so we know which entries
        // came from THIS run rather than a previous one.
        let probe_started = chrono::Utc::now();

        let conn = Self::session_bus().await?;
        let scripting = zbus::Proxy::new(
            &conn,
            "org.kde.KWin",
            "/Scripting",
            "org.kde.kwin.Scripting",
        )
        .await?;
        let plugin_name = format!("loopctl-probe-{}", std::process::id());
        let path_str = path.to_string_lossy().to_string();
        let _: i32 = scripting
            .call("loadScript", &(path_str.clone(), plugin_name.clone()))
            .await
            .context("KWin loadScript (probe)")?;
        let _: () = scripting.call("start", &()).await.context("KWin start (probe)")?;
        sleep(Duration::from_millis(200)).await;
        let _: bool = scripting
            .call("unloadScript", &(plugin_name,))
            .await
            .unwrap_or(false);
        let _ = std::fs::remove_file(&path);

        // Scrape the journal for our marker. We only look at lines newer
        // than `probe_started` to avoid picking up stale runs.
        let since_arg = probe_started
            .with_timezone(&chrono::Local)
            .format("%Y-%m-%d %H:%M:%S")
            .to_string();
        let output = tokio::process::Command::new("journalctl")
            .arg("--user")
            .arg("--since")
            .arg(&since_arg)
            .arg("--no-pager")
            .arg("-q")
            .output()
            .await
            .context("run journalctl --user")?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let mut counts: HashMap<u32, u32> = HashMap::new();
        for line in stdout.lines() {
            if let Some(idx) = line.find(&marker) {
                let rest = &line[idx + marker.len()..];
                let mut parts = rest.split_whitespace();
                let Some(d) = parts.next().and_then(|s| s.parse::<u32>().ok()) else {
                    continue;
                };
                let Some(c) = parts.next().and_then(|s| s.parse::<u32>().ok()) else {
                    continue;
                };
                counts.insert(d, c);
            }
        }
        Ok(counts)
    }
}

/// Single-quote escape for `cd <dir>` paths. We do NOT call out to a real
/// shell quoter crate yet — the workdir is a known PathBuf so the input is
/// not user-supplied free-text.
fn shell_escape(s: &str) -> String {
    let escaped = s.replace('\'', "'\\''");
    format!("'{escaped}'")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pane_id_round_trip() {
        let id = make_pane_id(12345, 7);
        assert_eq!(id.as_str(), "konsole:12345:7");
        let (pid, sid) = parse_pane_id(&id).unwrap();
        assert_eq!((pid, sid), (12345, 7));
    }

    #[test]
    fn pane_id_rejects_non_konsole() {
        let bad = PaneId::new("tmux:s:0");
        assert!(parse_pane_id(&bad).is_err());
    }

    #[test]
    fn bus_name_format() {
        assert_eq!(bus_name_from_pid(42), "org.kde.konsole-42");
    }

    #[test]
    fn pid_from_bus_round_trip() {
        assert_eq!(KonsolePlugin::pid_from_bus("org.kde.konsole-42"), Some(42));
        assert_eq!(KonsolePlugin::pid_from_bus("org.kde.konsole-bogus"), None);
        assert_eq!(KonsolePlugin::pid_from_bus("org.kde.other"), None);
    }

    #[test]
    fn shell_escape_handles_quotes_and_spaces() {
        assert_eq!(shell_escape("/tmp/foo bar"), "'/tmp/foo bar'");
        assert_eq!(shell_escape("o'malley"), "'o'\\''malley'");
    }
}