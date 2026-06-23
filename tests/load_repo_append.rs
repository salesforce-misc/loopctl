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

//! Integration tests for `load_repo` in the append-on-existing mode.

use std::path::Path;
use std::sync::Arc;

use agent_loop_workbench::repo_loader::{load_repo, LoadAction};
use agent_loop_workbench::terminal::mock::MockTerminalPlugin;
use agent_loop_workbench::terminal::TerminalPlugin;

/// Tell `load_repo` not to touch the developer's real
/// ~/.config/loopctl/config.toml. Set unconditionally for every test
/// in this crate; safe to set repeatedly.
///
/// SAFETY: edition-2024 made set_var unsafe because it races with
/// concurrent readers on glibc. This env var is only consulted by
/// `load_repo` which is awaited inside the same test, so no concurrent
/// reader exists at the moment we set it.
fn inhibit_user_config() {
    unsafe { std::env::set_var("LOOPCTL_TEST_INHIBIT_USER_CONFIG", "1") };
}

fn write_repo(root: &Path, headings_to_dirs: &[(&str, &str)]) {
    std::fs::create_dir_all(root).unwrap();
    let mut readme = String::from("# repo\n\n");
    for (heading, dir) in headings_to_dirs {
        std::fs::create_dir_all(root.join(dir)).unwrap();
        readme.push_str(&format!("### {heading}\n\n```\nresume body\n```\n\n"));
    }
    std::fs::write(root.join("README.md"), readme).unwrap();
}

#[tokio::test]
async fn first_load_generates_full_workbench_toml() {
    inhibit_user_config();
    let dir = tempfile::tempdir().unwrap();
    write_repo(
        dir.path(),
        &[
            ("alpha — first loop", "alpha"),
            ("beta — second loop", "beta"),
        ],
    );
    let plugin: Arc<dyn TerminalPlugin> = Arc::new(MockTerminalPlugin::new());
    let outcome = load_repo(dir.path().to_str().unwrap(), plugin.as_ref())
        .await
        .unwrap();
    match outcome.action {
        LoadAction::Generated => {}
        other => panic!("expected Generated, got {other:?}"),
    }
    assert_eq!(outcome.discovered.len(), 2);
    let body = std::fs::read_to_string(dir.path().join("workbench.toml")).unwrap();
    assert!(body.contains("name             = \"alpha\""));
    assert!(body.contains("name             = \"beta\""));
}

#[tokio::test]
async fn second_load_appends_only_new_loops() {
    inhibit_user_config();
    let dir = tempfile::tempdir().unwrap();
    write_repo(dir.path(), &[("alpha — first loop", "alpha")]);
    let plugin: Arc<dyn TerminalPlugin> = Arc::new(MockTerminalPlugin::new());
    let first = load_repo(dir.path().to_str().unwrap(), plugin.as_ref())
        .await
        .unwrap();
    assert!(matches!(first.action, LoadAction::Generated));

    // Now add a NEW loop on disk + README; re-load should append it.
    std::fs::create_dir_all(dir.path().join("beta")).unwrap();
    std::fs::write(
        dir.path().join("README.md"),
        "# repo\n\n### alpha — first loop\n\n```\nbody\n```\n\n### beta — second\n\n```\nbody\n```\n",
    )
    .unwrap();
    let second = load_repo(dir.path().to_str().unwrap(), plugin.as_ref())
        .await
        .unwrap();
    match second.action {
        LoadAction::Appended { added } => assert_eq!(added, 1),
        other => panic!("expected Appended, got {other:?}"),
    }
    let body = std::fs::read_to_string(dir.path().join("workbench.toml")).unwrap();
    // Both loops present; alpha's stanza should appear exactly once.
    let alpha_count = body.matches("name             = \"alpha\"").count();
    assert_eq!(alpha_count, 1, "alpha should not be duplicated");
    assert!(body.contains("name             = \"beta\""));
    // Append marker comment present
    assert!(body.contains("appended by `loopctl load-repo`"));
}

#[tokio::test]
async fn third_load_with_no_new_loops_is_up_to_date() {
    inhibit_user_config();
    let dir = tempfile::tempdir().unwrap();
    write_repo(dir.path(), &[("alpha — only", "alpha")]);
    let plugin: Arc<dyn TerminalPlugin> = Arc::new(MockTerminalPlugin::new());
    load_repo(dir.path().to_str().unwrap(), plugin.as_ref())
        .await
        .unwrap();
    let again = load_repo(dir.path().to_str().unwrap(), plugin.as_ref())
        .await
        .unwrap();
    assert!(matches!(again.action, LoadAction::UpToDate));
}

#[tokio::test]
async fn handcrafted_workbench_toml_is_preserved_on_append() {
    inhibit_user_config();
    let dir = tempfile::tempdir().unwrap();
    write_repo(
        dir.path(),
        &[
            ("alpha — first", "alpha"),
            ("beta — second", "beta"),
        ],
    );
    // User wrote workbench.toml by hand with custom schedule + tick_args
    // for alpha. Don't include beta — that's the gap we fill.
    let handcrafted = r#"
schema_version = 1

[workbench]
claude_loops_dir = "~/whatever"
terminal = "konsole"
central_group = "1"

[[loops]]
name             = "alpha"
dir              = "alpha"
readme_anchor    = "alpha--first"
group_key        = "7"
schedule         = "every 10m"
dispatch         = "tick_script"
tick_script      = "skill/scripts/tick.py"
tick_args        = ["--cache", "/tmp/x.json"]
"#;
    std::fs::write(dir.path().join("workbench.toml"), handcrafted).unwrap();

    let plugin: Arc<dyn TerminalPlugin> = Arc::new(MockTerminalPlugin::new());
    let outcome = load_repo(dir.path().to_str().unwrap(), plugin.as_ref())
        .await
        .unwrap();
    match outcome.action {
        LoadAction::Appended { added } => assert_eq!(added, 1),
        other => panic!("expected Appended, got {other:?}"),
    }
    let body = std::fs::read_to_string(dir.path().join("workbench.toml")).unwrap();
    // Alpha's hand-written stanza must be untouched.
    assert!(body.contains("schedule         = \"every 10m\""));
    assert!(body.contains("tick_args"));
    assert!(body.contains("/tmp/x.json"));
    // Beta got appended
    assert!(body.contains("name             = \"beta\""));
}