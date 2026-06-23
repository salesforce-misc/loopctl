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

//! Golden tests for metric extraction against frozen real-world tracker.md
//! and progress.md fixtures (copies of live ~/claude-loops files).
//! Refresh from upstream when schemas drift.

use std::path::PathBuf;

use agent_loop_workbench::metrics::extractors;

fn fixture(name: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path:?}: {e}"))
}

#[test]
fn alpha_loop_tracker_active_section_has_three_rows() {
    let md = fixture("alpha-loop-tracker.md");
    let n = extractors::count_rows_under_heading(&md, "active-change-cases-pr-still-open")
        .expect("section exists");
    assert!(n >= 3, "expected at least 3 active cases, got {n}");
}

#[test]
fn alpha_loop_tracker_missing_section_lists_pretty_long() {
    let md = fixture("alpha-loop-tracker.md");
    let n = extractors::count_rows_under_heading(&md, "prs-missing-a-change-case")
        .expect("section exists");
    assert!(
        n >= 10,
        "expected the missing-CC list to have ≥10 rows; got {n}"
    );
}

#[test]
fn alpha_loop_tracker_pending_close_section_present() {
    let md = fixture("alpha-loop-tracker.md");
    let n = extractors::count_rows_under_heading(
        &md,
        "change-cases-pending-close-pr-already-mergedclosed",
    )
    .expect("section exists");
    assert!(n >= 5, "expected ≥5 pending-close rows, got {n}");
}

#[test]
fn alpha_loop_progress_has_no_phase_marker() {
    // The fixture's progress.md doesn't track phases; absence is OK.
    let md = fixture("alpha-loop-progress.md");
    assert_eq!(extractors::find_phase_marker(&md), None);
}

#[test]
fn synthetic_progress_with_phase_marker_works() {
    let md = "## Status\n\nCurrent phase: 4 of 6\n";
    assert_eq!(extractors::find_phase_marker(md), Some((4, 6)));
}