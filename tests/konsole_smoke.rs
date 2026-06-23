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

// Step 2 smoke test for the Konsole TerminalPlugin implementation.
//
// Manual run only — requires a live KDE Plasma X11 session and `konsole`
// on PATH. Skipped automatically by `cargo test`. Run with:
//
//   cargo test --test konsole_smoke -- --ignored --test-threads=1
//
// Asserts end-to-end:
//   1. ensure_group spawns a fresh konsole on a brand-new bus name.
//   2. ensure_pane is idempotent on (group, title).
//   3. The pasted text actually arrives in the session — verified by
//      having the pane shell `head -1 > <tmpfile>` and reading the file.
//   4. close() closes the pane cleanly.

use std::path::PathBuf;
use std::time::Duration;

use agent_loop_workbench::terminal::{
    konsole::KonsolePlugin, pane_spec, TerminalPlugin,
};
use tokio::time::{sleep, Instant};

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires live KDE Plasma X11 session"]
async fn end_to_end_spawn_paste_close() {
    let tmp = tempfile::NamedTempFile::new().expect("tmpfile");
    let tmp_path = tmp.path().to_path_buf();
    let done_path = PathBuf::from(format!("{}.done", tmp_path.display()));
    let _ = std::fs::remove_file(&done_path);

    let plugin = KonsolePlugin::new(None);
    plugin
        .supports_environment()
        .expect("environment must support konsole");

    plugin.ensure_group("smoke").await.expect("ensure_group");

    let spec = pane_spec(
        "smoke",
        "konsole-smoke-tab",
        std::env::var("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("/tmp")),
        Some(format!(
            "stty -echo; head -1 > {} ; echo done > {}",
            tmp_path.display(),
            done_path.display()
        )),
    );

    let id_1 = plugin.ensure_pane(&spec).await.expect("ensure_pane #1");
    let id_2 = plugin.ensure_pane(&spec).await.expect("ensure_pane #2");
    assert_eq!(id_1, id_2, "ensure_pane must be idempotent");

    plugin
        .paste(&id_1, "hello-from-trait")
        .await
        .expect("paste");

    let deadline = Instant::now() + Duration::from_secs(5);
    while !done_path.exists() {
        assert!(Instant::now() < deadline, "pasted command never completed");
        sleep(Duration::from_millis(100)).await;
    }
    let body = std::fs::read_to_string(&tmp_path).expect("read tmpfile");
    assert_eq!(body.trim(), "hello-from-trait");

    plugin.close(&id_1).await.expect("close");
    let _ = std::fs::remove_file(&done_path);
}