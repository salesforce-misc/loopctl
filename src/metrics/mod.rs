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

//! Heuristic metric extraction from `progress.md` and `tracker.md`.
//!
//! All extractors are pure functions on a string. The TUI calls
//! [`extract_for_loop`] which inspects what files exist in a loop's
//! directory and runs the right combination.

pub mod extractors;

use std::path::Path;

use chrono::{DateTime, TimeZone, Utc};

use crate::state::Metrics;

/// Run the heuristic suite for one loop. Looks for tracker.md / progress.md
/// in `loop_dir` and pulls whatever it can. Missing files are silently
/// skipped.
pub fn extract_for_loop(loop_dir: &Path) -> Metrics {
    let mut m = Metrics::default();
    let tracker_path = loop_dir.join("tracker.md");
    if let Ok(body) = std::fs::read_to_string(&tracker_path) {
        // Section row counts. Hard-coded heading set covers the schemas in
        // the user's existing trackers — extra unmatched headings are ignored.
        for (label, name) in [
            ("active_change_cases", "active-change-cases-pr-still-open"),
            ("missing_change_cases", "prs-missing-a-change-case"),
            ("pending_close", "change-cases-pending-close-pr-already-mergedclosed"),
            ("active_no_pr", "active-cases-needing-manual-review-no-pr-url"),
            ("closed_change_cases", "closed-change-cases"),
            ("excluded_from_missing", "excluded-from-missing-list-out-of-scope-wont-need-a-change-case"),
            // Generic fallbacks that other loops use:
            ("tracker_table_rows", "tracker"),
            ("active_wis", "active-work-items"),
            ("recently_closed", "recently-closed"),
        ] {
            if let Some(n) = extractors::count_rows_under_heading(&body, name) {
                m.integers.insert(label.to_string(), n as i64);
            }
        }
        if let Some(mt) = mtime_utc(&tracker_path) {
            m.mtimes.insert("tracker_md".into(), mt);
        }
    }
    let progress_path = loop_dir.join("progress.md");
    if let Ok(body) = std::fs::read_to_string(&progress_path) {
        if let Some((cur, total)) = extractors::find_phase_marker(&body) {
            m.integers.insert("phase".into(), cur as i64);
            m.integers.insert("phase_total".into(), total as i64);
        }
        let (checked, unchecked) = extractors::count_checkboxes(&body);
        if checked + unchecked > 0 {
            m.integers.insert("checked".into(), checked as i64);
            m.integers.insert("unchecked".into(), unchecked as i64);
        }
        for key in ["next-target", "baseline", "current-status"] {
            if let Some(v) = extractors::find_kv(&body, key) {
                m.strings
                    .insert(key.replace('-', "_"), truncate(&v, 120).to_string());
            }
        }
        if let Some(mt) = mtime_utc(&progress_path) {
            m.mtimes.insert("progress_md".into(), mt);
        }
    }
    m
}

fn mtime_utc(p: &Path) -> Option<DateTime<Utc>> {
    let m = std::fs::metadata(p).ok()?;
    let modified = m.modified().ok()?;
    let secs = modified
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs() as i64;
    Utc.timestamp_opt(secs, 0).single()
}

fn truncate(s: &str, n: usize) -> &str {
    let s = s.trim();
    if s.len() <= n {
        s
    } else {
        // Find a char boundary at or before n.
        let mut end = n;
        while !s.is_char_boundary(end) && end > 0 {
            end -= 1;
        }
        &s[..end]
    }
}