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

// Step 1 spike — drives Konsole over D-Bus end to end so we know the surface
// before we wrap it in the TerminalPlugin trait.
//
// Spawns `konsole --separate --workdir <DIR>` in its own process so it shows
// up as a fresh `org.kde.konsole-<pid>` bus name, then over D-Bus:
//   1. discovers the new bus name (snapshot diff, not pid-guessing)
//   2. sets the first session's title to <TITLE>
//   3. runs <COMMAND> (default: claude) in that session
//   4. waits 1.5 s for the command to be ready
//   5. sends <PASTE> + "\n" via Session.sendText (raw bytes, no shell quoting)
//
// Usage:
//   konsole-spike --workdir DIR --tab-title TITLE [--command CMD] [--paste TEXT]
//   konsole-spike --attach BUS_NAME --tab-title TITLE [...]   # skip spawn
//
// Window placement onto a specific virtual desktop is intentionally NOT in
// this spike — the qdbus surface for KWin doesn't expose a per-window
// setOnDesktop, so that risk is handled in step 2 via xdo / EWMH and tracked
// separately in the plan.

use anyhow::{anyhow, bail, Context, Result};
use std::collections::HashSet;
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;
use tokio::time::{sleep, Instant};
use zbus::fdo::DBusProxy;
use zbus::{proxy, Connection};

#[proxy(interface = "org.kde.konsole.Window", default_path = "/Windows/1")]
trait KonsoleWindow {
    // QStringList — Konsole returns session ids as decimal strings, e.g.
    // ["1", "2"]. Parse callers convert to i32.
    #[zbus(name = "sessionList")]
    fn session_list(&self) -> zbus::Result<Vec<String>>;
    #[zbus(name = "currentSession")]
    fn current_session(&self) -> zbus::Result<i32>;
}

#[proxy(interface = "org.kde.konsole.Session")]
trait KonsoleSession {
    #[zbus(name = "runCommand")]
    fn run_command(&self, command: &str) -> zbus::Result<()>;
    #[zbus(name = "sendText")]
    fn send_text(&self, text: &str) -> zbus::Result<()>;
    #[zbus(name = "setTitle")]
    fn set_title(&self, role: i32, title: &str) -> zbus::Result<()>;
    #[zbus(name = "processId")]
    fn process_id(&self) -> zbus::Result<i32>;
}

#[derive(Debug)]
struct Args {
    workdir: Option<String>,
    tab_title: String,
    command: Option<String>,
    paste: Option<String>,
    attach: Option<String>,
}

fn parse_args() -> Result<Args> {
    let mut args = std::env::args().skip(1);
    let mut workdir = None;
    let mut tab_title = "spike".to_string();
    let mut command = None;
    let mut paste = None;
    let mut attach = None;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--workdir" => workdir = args.next(),
            "--tab-title" => {
                tab_title = args
                    .next()
                    .ok_or_else(|| anyhow!("--tab-title needs a value"))?
            }
            "--command" => command = args.next(),
            "--paste" => paste = args.next(),
            "--attach" => attach = args.next(),
            "-h" | "--help" => {
                print_help();
                std::process::exit(0);
            }
            other => bail!("unknown arg: {other}"),
        }
    }
    Ok(Args {
        workdir,
        tab_title,
        command,
        paste,
        attach,
    })
}

fn print_help() {
    eprintln!(
        "konsole-spike — exercise Konsole D-Bus end to end\n\n\
         USAGE:\n\
         \x20 konsole-spike --workdir DIR --tab-title TITLE [--command CMD] [--paste TEXT]\n\
         \x20 konsole-spike --attach BUS_NAME --tab-title TITLE [...]\n"
    );
}

async fn list_konsole_bus_names(conn: &Connection) -> Result<HashSet<String>> {
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

async fn wait_for_new_bus_name(
    conn: &Connection,
    before: &HashSet<String>,
    timeout_secs: u64,
) -> Result<String> {
    let deadline = Instant::now() + Duration::from_secs(timeout_secs);
    loop {
        let now = list_konsole_bus_names(conn).await?;
        if let Some(new) = now.difference(before).next() {
            return Ok(new.clone());
        }
        if Instant::now() >= deadline {
            bail!("timed out waiting for new org.kde.konsole-* bus name");
        }
        sleep(Duration::from_millis(100)).await;
    }
}

async fn ensure_window_path(conn: &Connection, dest: &str) -> Result<String> {
    // Konsole windows live at /Windows/N starting at 1; for a freshly spawned
    // konsole there's exactly one. For attach-mode we still expect /Windows/1
    // for the first window. Verifying it exists is cheap.
    let proxy = KonsoleWindowProxy::builder(conn)
        .destination(dest.to_string())?
        .path("/Windows/1")?
        .build()
        .await?;
    let _ = proxy.session_list().await.context(
        "konsole at /Windows/1 did not respond — try --attach to a known window path",
    )?;
    Ok("/Windows/1".to_string())
}

async fn first_session_id(conn: &Connection, dest: &str, window_path: &str) -> Result<i32> {
    let proxy = KonsoleWindowProxy::builder(conn)
        .destination(dest.to_string())?
        .path(window_path.to_string())?
        .build()
        .await?;
    let sessions = proxy.session_list().await?;
    let raw = sessions
        .into_iter()
        .next()
        .ok_or_else(|| anyhow!("konsole window has no sessions"))?;
    raw.parse::<i32>()
        .with_context(|| format!("session id {raw:?} is not an integer"))
}

fn normalize_paste(text: &str) -> String {
    // Claude is sensitive to bare CR — normalize all line endings to \n.
    text.replace("\r\n", "\n").replace('\r', "\n")
}

async fn drive_session(
    conn: &Connection,
    dest: &str,
    session_id: i32,
    args: &Args,
) -> Result<()> {
    let session_path = format!("/Sessions/{session_id}");
    let session = KonsoleSessionProxy::builder(conn)
        .destination(dest.to_string())?
        .path(session_path)?
        .build()
        .await?;

    // Set the title BEFORE running anything — it's our stable identity.
    // Role 0 = local title, role 1 = remote (ssh). For the spike, set both.
    session.set_title(0, &args.tab_title).await?;
    session.set_title(1, &args.tab_title).await?;

    if let Some(cmd) = &args.command {
        session.run_command(cmd).await?;
        // Give the command a beat to take over the foreground process.
        // Step 2 will replace this with a foreground-PID poll on the
        // Session.processId() before/after diff.
        sleep(Duration::from_millis(1500)).await;
    }

    if let Some(paste) = &args.paste {
        let mut text = normalize_paste(paste);
        if !text.ends_with('\n') {
            text.push('\n');
        }
        session.send_text(&text).await?;
    }
    Ok(())
}

async fn spawn_konsole(workdir: &str) -> Result<()> {
    // --separate is mandatory: without it konsole reuses the existing
    // process and we'd never see a new bus name.
    Command::new("konsole")
        .arg("--separate")
        .arg("--workdir")
        .arg(workdir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("failed to spawn konsole")?;
    Ok(())
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_target(false)
        .init();

    let args = parse_args()?;
    let conn = Connection::session().await.context("connect to session bus")?;

    let dest = match &args.attach {
        Some(name) => {
            tracing::info!(bus = %name, "attaching to existing konsole");
            name.clone()
        }
        None => {
            let workdir = args
                .workdir
                .clone()
                .unwrap_or_else(|| std::env::var("HOME").unwrap_or_else(|_| ".".into()));
            tracing::info!(workdir = %workdir, "spawning konsole");
            let before = list_konsole_bus_names(&conn).await?;
            spawn_konsole(&workdir).await?;
            wait_for_new_bus_name(&conn, &before, 5).await?
        }
    };

    let window_path = ensure_window_path(&conn, &dest).await?;
    let session_id = first_session_id(&conn, &dest, &window_path).await?;
    tracing::info!(dest = %dest, %window_path, session_id, "driving session");
    drive_session(&conn, &dest, session_id, &args).await?;
    tracing::info!("done");
    Ok(())
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
}