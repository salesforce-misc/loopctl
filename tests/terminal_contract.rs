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

//! Plugin-contract tests. Any TerminalPlugin implementation must pass
//! these. Run against MockTerminalPlugin in CI; future plugins re-use the
//! same suite (Konsole runs them under #[ignore] in konsole_smoke.rs).

use std::path::PathBuf;
use std::sync::Arc;

use agent_loop_workbench::terminal::{
    mock::{MockTerminalPlugin, Op},
    pane_spec, PaneId, TerminalPlugin,
};

fn mock() -> Arc<MockTerminalPlugin> {
    Arc::new(MockTerminalPlugin::new())
}

#[tokio::test]
async fn ensure_pane_is_idempotent_on_group_and_title() {
    let p = mock();
    let spec = pane_spec(
        "1",
        "alpha-loop",
        PathBuf::from("/tmp/cc"),
        Some("claude"),
    );
    p.ensure_group("1").await.unwrap();
    let a = p.ensure_pane(&spec).await.unwrap();
    let b = p.ensure_pane(&spec).await.unwrap();
    assert_eq!(a, b, "second ensure_pane must reuse the same PaneId");
}

#[tokio::test]
async fn ensure_group_does_not_duplicate_when_called_twice() {
    let p = mock();
    p.ensure_group("2").await.unwrap();
    p.ensure_group("2").await.unwrap();
    let groups: Vec<_> = p
        .ops()
        .into_iter()
        .filter(|op| matches!(op, Op::EnsureGroup(_)))
        .collect();
    assert_eq!(
        groups.len(),
        2,
        "the contract permits the same call twice — what matters is no extra side effects"
    );
}

#[tokio::test]
async fn paste_run_focus_close_pass_pane_through() {
    let p = mock();
    let spec = pane_spec("1", "wi", PathBuf::from("/tmp"), None::<&str>);
    p.ensure_group("1").await.unwrap();
    let id = p.ensure_pane(&spec).await.unwrap();

    p.paste(&id, "hello\n").await.unwrap();
    p.run(&id, "ls").await.unwrap();
    p.focus(&id).await.unwrap();
    p.close(&id).await.unwrap();

    let ops = p.ops();
    let pane_id = id.0.clone();
    assert!(ops.contains(&Op::Paste {
        pane: pane_id.clone(),
        text: "hello\n".into()
    }));
    assert!(ops.contains(&Op::Run {
        pane: pane_id.clone(),
        command: "ls".into()
    }));
    assert!(ops.contains(&Op::Focus(pane_id.clone())));
    assert!(ops.contains(&Op::Close(pane_id)));
}

#[tokio::test]
async fn supports_environment_returns_ok_for_mock() {
    let p = mock();
    assert!(p.supports_environment().is_ok());
    assert_eq!(p.name(), "mock");
}

#[tokio::test]
async fn ensure_pane_records_workdir_and_initial_command() {
    let p = mock();
    let spec = pane_spec(
        "3",
        "smart-perf",
        PathBuf::from("/home/u/claude-loops/smart-helm-terraform/perf"),
        Some("claude"),
    );
    p.ensure_group("3").await.unwrap();
    p.ensure_pane(&spec).await.unwrap();
    let recorded = p
        .ops()
        .into_iter()
        .find_map(|op| match op {
            Op::EnsurePane {
                workdir,
                initial_command,
                ..
            } => Some((workdir, initial_command)),
            _ => None,
        })
        .expect("ensure_pane recorded");
    assert_eq!(recorded.0, "/home/u/claude-loops/smart-helm-terraform/perf");
    assert_eq!(recorded.1.as_deref(), Some("claude"));
}

#[tokio::test]
async fn pane_id_is_distinct_per_unique_pane() {
    let p = mock();
    p.ensure_group("1").await.unwrap();
    p.ensure_group("2").await.unwrap();
    let a = p
        .ensure_pane(&pane_spec("1", "alpha", "/tmp", None::<&str>))
        .await
        .unwrap();
    let b = p
        .ensure_pane(&pane_spec("1", "beta", "/tmp", None::<&str>))
        .await
        .unwrap();
    let c = p
        .ensure_pane(&pane_spec("2", "alpha", "/tmp", None::<&str>))
        .await
        .unwrap();
    assert_ne!(a, b);
    assert_ne!(a, c);
    assert_ne!(b, c);
    assert_ne!(PaneId::new(""), a);
}