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

//! Tick dispatcher. Given a config + plugin + state, fire one tick for a
//! single loop and update state accordingly.
//!
//! `dispatch = "tick_script"` ⇒ run the script as a subprocess (cwd = loop
//! dir). Capture exit code; the script's own stdout/stderr go to the
//! daemon's log via inherit-then-tee.
//!
//! `dispatch = "resume_prompt"` ⇒ paste the loop's resume prompt into its
//! pane (re-extracts from README.md every time so README edits show up
//! without a restart).
//!
//! `dispatch = "none"` ⇒ refuse, since a manual `loopctl tick <name>` is
//! the only way to fire and it shouldn't be silent.

use std::path::Path;
use std::sync::Arc;

use anyhow::{anyhow, bail, Context, Result};

use crate::config::{Dispatch, LoopConfig, WorkbenchConfig};
use crate::readme;
use crate::state::GlobalState;
use crate::terminal::{PaneId, TerminalPlugin};

/// Result of a single tick. Captured into state.
#[derive(Debug, Clone)]
pub struct TickReport {
    pub kind: &'static str,
    /// Exit code for `tick_script`; always `Some(0)` for `resume_prompt`.
    pub exit: Option<i32>,
}

/// Fire a single tick for `name`. Updates `state.loops[name].last_tick`,
/// `last_dispatch_kind`, `last_dispatch_exit`, and recomputes `next_tick`.
pub async fn tick_one(
    config: &WorkbenchConfig,
    plugin: Arc<dyn TerminalPlugin>,
    state: &mut GlobalState,
    name: &str,
) -> Result<TickReport> {
    let lc = config
        .loop_by_name(name)
        .ok_or_else(|| anyhow!("unknown loop `{name}`"))?;
    if !lc.active {
        anyhow::bail!(
            "loop `{name}` is inactive — run `loopctl reopen {name}` to reactivate it"
        );
    }

    let now = chrono::Utc::now();
    let report = match lc.dispatch {
        Dispatch::None => bail!(
            "loop `{}` has dispatch = `none`; nothing to fire",
            lc.name
        ),
        Dispatch::TickScript => run_tick_script(config, lc).await?,
        Dispatch::ResumePrompt => paste_resume_prompt(config, plugin, state, lc).await?,
    };

    // Refresh heuristic metrics from disk after the dispatch — tick_script
    // loops will have just rewritten tracker.md/progress.md, and even
    // resume_prompt loops benefit from a fresh mtime.
    let loops_dir = crate::util::expand_path(&config.workbench.claude_loops_dir);
    let loop_dir = Path::new(&loops_dir).join(&lc.dir);
    let metrics = crate::metrics::extract_for_loop(&loop_dir);

    let entry = state.loops.entry(lc.name.clone()).or_default();
    entry.last_tick = Some(now);
    entry.last_dispatch_kind = Some(report.kind.to_string());
    entry.last_dispatch_exit = report.exit;
    entry.last_metrics = metrics;
    let schedule = lc
        .parsed_schedule()
        .with_context(|| format!("re-parsing schedule for {}", lc.name))?;
    entry.next_tick = crate::scheduler::next_fire(&schedule, now, Some(now));
    Ok(report)
}

async fn run_tick_script(config: &WorkbenchConfig, lc: &LoopConfig) -> Result<TickReport> {
    let loops_dir = crate::util::expand_path(&config.workbench.claude_loops_dir);
    let loops_dir = Path::new(&loops_dir);
    let loop_dir = loops_dir.join(&lc.dir);
    let script_rel = lc
        .tick_script
        .as_deref()
        .ok_or_else(|| anyhow!("loop `{}` missing tick_script", lc.name))?;
    let script_path = loop_dir.join(script_rel);
    if !script_path.exists() {
        bail!(
            "tick_script for loop `{}` does not exist: {script_path:?}",
            lc.name
        );
    }
    let args: Vec<String> = lc
        .tick_args
        .iter()
        .map(|a| crate::util::expand_path(a))
        .collect();
    tracing::info!(name = %lc.name, ?script_path, ?args, "running tick_script");
    let status = tokio::process::Command::new("python3")
        .arg(&script_path)
        .args(&args)
        .current_dir(&loop_dir)
        .status()
        .await
        .with_context(|| format!("spawn python3 {script_path:?}"))?;
    Ok(TickReport {
        kind: "tick_script",
        exit: status.code(),
    })
}

async fn paste_resume_prompt(
    config: &WorkbenchConfig,
    plugin: Arc<dyn TerminalPlugin>,
    state: &GlobalState,
    lc: &LoopConfig,
) -> Result<TickReport> {
    let loops_dir = crate::util::expand_path(&config.workbench.claude_loops_dir);
    let loops_dir = Path::new(&loops_dir);
    let readme_path = loops_dir.join("README.md");

    let pane_id = state
        .loops
        .get(&lc.name)
        .and_then(|s| s.pane_id.clone())
        .ok_or_else(|| {
            anyhow!(
                "loop `{}` has no pane (run `loopctl up` before ticking)",
                lc.name
            )
        })?;
    let prompt = readme::extract_from_path(&readme_path, &lc.readme_anchor)?;
    plugin.paste(&PaneId(pane_id), &prompt).await?;
    Ok(TickReport {
        kind: "resume_prompt",
        exit: Some(0),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config;
    use crate::terminal::mock::{MockTerminalPlugin, Op};

    fn write_dirs(root: &Path) {
        std::fs::create_dir_all(root.join("alpha-dir/skill/scripts")).unwrap();
        std::fs::create_dir_all(root.join("beta-dir")).unwrap();
        let readme = "\
### alpha — label

```
resume alpha
```

### beta — label

```
resume beta
```
";
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
schedule = "every 10m"
dispatch = "tick_script"
tick_script = "skill/scripts/tick.py"

[[loops]]
name = "beta"
dir = "beta-dir"
readme_anchor = "beta--label"
group_key = "2"
schedule = "every 5m"
dispatch = "resume_prompt"
"#,
            loops_dir.display()
        );
        config::parse(&body).unwrap()
    }

    #[tokio::test]
    async fn tick_script_runs_subprocess_and_records_exit() {
        let dir = tempfile::tempdir().unwrap();
        write_dirs(dir.path());
        // Write a tick.py that exits 0 and creates a sentinel file.
        let sentinel = dir.path().join("tick.sentinel");
        let py = format!(
            "import pathlib\npathlib.Path({:?}).write_text('ok')\n",
            sentinel.to_string_lossy()
        );
        std::fs::write(
            dir.path().join("alpha-dir/skill/scripts/tick.py"),
            py,
        )
        .unwrap();

        let cfg = cfg_for(dir.path());
        let plugin = Arc::new(MockTerminalPlugin::new());
        let mut state = GlobalState::default();
        let report = tick_one(&cfg, plugin.clone(), &mut state, "alpha")
            .await
            .unwrap();
        assert_eq!(report.kind, "tick_script");
        assert_eq!(report.exit, Some(0));
        assert!(sentinel.exists(), "tick.py should have written sentinel");
        let s = state.loops.get("alpha").unwrap();
        assert!(s.last_tick.is_some());
        assert!(s.next_tick.is_some()); // every 10m
        assert_eq!(s.last_dispatch_kind.as_deref(), Some("tick_script"));
    }

    #[tokio::test]
    async fn tick_script_records_nonzero_exit() {
        let dir = tempfile::tempdir().unwrap();
        write_dirs(dir.path());
        std::fs::write(
            dir.path().join("alpha-dir/skill/scripts/tick.py"),
            "import sys; sys.exit(7)",
        )
        .unwrap();
        let cfg = cfg_for(dir.path());
        let plugin = Arc::new(MockTerminalPlugin::new());
        let mut state = GlobalState::default();
        let report = tick_one(&cfg, plugin, &mut state, "alpha")
            .await
            .unwrap();
        assert_eq!(report.exit, Some(7));
    }

    #[tokio::test]
    async fn resume_prompt_dispatch_pastes_into_existing_pane() {
        let dir = tempfile::tempdir().unwrap();
        write_dirs(dir.path());
        let cfg = cfg_for(dir.path());
        let plugin = Arc::new(MockTerminalPlugin::new());
        let mut state = GlobalState::default();
        // Pretend `up` already ran for beta and registered a pane.
        let entry = state.loops.entry("beta".into()).or_default();
        entry.pane_id = Some("mock:42".into());

        let report = tick_one(&cfg, plugin.clone(), &mut state, "beta")
            .await
            .unwrap();
        assert_eq!(report.kind, "resume_prompt");
        assert_eq!(report.exit, Some(0));

        let pasted: Vec<_> = plugin
            .ops()
            .into_iter()
            .filter_map(|op| match op {
                Op::Paste { pane, text } if pane == "mock:42" => Some(text),
                _ => None,
            })
            .collect();
        assert_eq!(pasted.len(), 1);
        assert!(pasted[0].contains("resume beta"));
    }

    #[tokio::test]
    async fn resume_prompt_without_pane_errors() {
        let dir = tempfile::tempdir().unwrap();
        write_dirs(dir.path());
        let cfg = cfg_for(dir.path());
        let plugin = Arc::new(MockTerminalPlugin::new());
        let mut state = GlobalState::default();
        let err = tick_one(&cfg, plugin, &mut state, "beta")
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("no pane"));
    }

    #[tokio::test]
    async fn dispatch_none_refuses_to_fire() {
        let dir = tempfile::tempdir().unwrap();
        write_dirs(dir.path());
        let mut cfg = cfg_for(dir.path());
        cfg.loops[1].dispatch = Dispatch::None;
        cfg.loops[1].tick_script = None;
        let plugin = Arc::new(MockTerminalPlugin::new());
        let mut state = GlobalState::default();
        let err = tick_one(&cfg, plugin, &mut state, "beta")
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("dispatch = `none`"));
    }

    #[tokio::test]
    async fn unknown_loop_errors() {
        let dir = tempfile::tempdir().unwrap();
        write_dirs(dir.path());
        let cfg = cfg_for(dir.path());
        let plugin = Arc::new(MockTerminalPlugin::new());
        let mut state = GlobalState::default();
        let err = tick_one(&cfg, plugin, &mut state, "nope")
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("unknown loop"));
    }
}