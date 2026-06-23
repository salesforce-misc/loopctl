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

//! Pure heuristics. Each function takes a markdown body and returns
//! an Option<...>. Designed for stability across the user's existing
//! tracker.md / progress.md schemas. Extra heuristics can be added
//! without breaking callers.

use crate::readme::slugify_heading;

/// Count rows in the first markdown table that follows the heading whose
/// GFM name equals `section_slug`. Skips the header + separator rows.
/// Returns `None` if no matching heading or no table is found.
pub fn count_rows_under_heading(md: &str, section_slug: &str) -> Option<usize> {
    let mut iter = md.lines();
    let mut found_heading = false;
    for line in iter.by_ref() {
        if let Some(rest) = strip_heading(line) {
            if slugify_heading(rest) == section_slug {
                found_heading = true;
                break;
            }
        }
    }
    if !found_heading {
        return None;
    }
    // Walk forward until we hit a `|` line; count consecutive `|` lines,
    // then subtract 2 (header + separator).
    let mut in_table = false;
    let mut count = 0usize;
    for line in iter {
        let is_table = line.trim_start().starts_with('|');
        let is_heading = line.starts_with('#');
        if is_heading && in_table {
            break;
        }
        if is_table {
            in_table = true;
            count += 1;
        } else if in_table && !line.trim().is_empty() {
            // Non-empty non-table line ends the table.
            break;
        }
    }
    if count >= 2 {
        Some(count - 2)
    } else {
        None
    }
}

fn strip_heading(line: &str) -> Option<&str> {
    for prefix in ["#### ", "### ", "## ", "# "] {
        if let Some(rest) = line.strip_prefix(prefix) {
            return Some(rest.trim());
        }
    }
    None
}

/// Find a "phase: N of M" or "phase: N/M" marker. Case-insensitive.
pub fn find_phase_marker(md: &str) -> Option<(u32, u32)> {
    // Hand-rolled scan — avoids pulling regex for one match.
    for line in md.lines() {
        let lc = line.to_lowercase();
        let trimmed = lc.trim_start();
        let rest = match trimmed.strip_prefix("current phase") {
            Some(r) => r,
            None => match trimmed.strip_prefix("phase") {
                Some(r) => r,
                None => continue,
            },
        };
        let after_sep = rest
            .trim_start()
            .strip_prefix(':')
            .or_else(|| rest.trim_start().strip_prefix('-'))
            .unwrap_or(rest);
        if let Some(pair) = parse_n_of_m(after_sep.trim()) {
            return Some(pair);
        }
    }
    None
}

fn parse_n_of_m(s: &str) -> Option<(u32, u32)> {
    // Accept "3 of 6", "3/6", "3 / 6"
    let s = s.trim();
    let (a, b) = if let Some((a, b)) = s.split_once("of") {
        (a, b)
    } else {
        s.split_once('/')?
    };
    let a: u32 = a.trim().parse().ok()?;
    let b: u32 = b.split_whitespace().next()?.parse().ok()?;
    Some((a, b))
}

/// Count `- [ ]` and `- [x]` checkboxes (returns `(checked, unchecked)`).
pub fn count_checkboxes(md: &str) -> (usize, usize) {
    let mut checked = 0;
    let mut unchecked = 0;
    for line in md.lines() {
        let t = line.trim_start();
        if t.starts_with("- [ ]") || t.starts_with("* [ ]") {
            unchecked += 1;
        } else if t.starts_with("- [x]")
            || t.starts_with("- [X]")
            || t.starts_with("* [x]")
            || t.starts_with("* [X]")
        {
            checked += 1;
        }
    }
    (checked, unchecked)
}

/// Find a key-value line like `next-target: foo`, `next-target = foo`,
/// or `- next-target: foo`. Case-insensitive on the key.
pub fn find_kv(md: &str, key: &str) -> Option<String> {
    let key_lower = key.to_lowercase();
    for line in md.lines() {
        let trimmed = line.trim_start();
        // Strip optional bullet prefix.
        let after_bullet = trimmed
            .strip_prefix("- ")
            .or_else(|| trimmed.strip_prefix("* "))
            .unwrap_or(trimmed);
        // Lowercase prefix only — preserve the value's original case.
        let lc_prefix: String = after_bullet
            .chars()
            .take(key_lower.len())
            .flat_map(|c| c.to_lowercase())
            .collect();
        if lc_prefix == key_lower {
            let rest = &after_bullet[key_lower.len()..];
            let rest = rest.trim_start();
            if let Some(v) = rest.strip_prefix(':').or_else(|| rest.strip_prefix('=')) {
                let v = v.trim();
                if !v.is_empty() {
                    return Some(v.to_string());
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn count_rows_simple() {
        let md = "\
## Active

| col | x |
|---|---|
| a | 1 |
| b | 2 |

## Other
";
        assert_eq!(count_rows_under_heading(md, "active"), Some(2));
    }

    #[test]
    fn count_rows_returns_none_for_missing_heading() {
        let md = "## Foo\n\n| a | b |\n|---|---|\n| 1 | 2 |\n";
        assert_eq!(count_rows_under_heading(md, "bar"), None);
    }

    #[test]
    fn count_rows_returns_none_for_heading_without_table() {
        let md = "## No table here\n\nJust prose.\n";
        assert_eq!(count_rows_under_heading(md, "no-table-here"), None);
    }

    #[test]
    fn phase_marker_matches_progress_text() {
        let md = "Current phase: 3 of 6\n";
        assert_eq!(find_phase_marker(md), Some((3, 6)));
        let md = "phase: 4/7\n";
        assert_eq!(find_phase_marker(md), Some((4, 7)));
    }

    #[test]
    fn phase_marker_returns_none_when_absent() {
        assert_eq!(find_phase_marker("nothing here"), None);
    }

    #[test]
    fn checkboxes_counted_both_styles() {
        let md = "\
- [ ] do A
- [x] did B
* [X] also did C
- [ ] another todo
";
        assert_eq!(count_checkboxes(md), (2, 2));
    }

    #[test]
    fn kv_finds_key_with_colon() {
        let md = "next-target: investigate flaky test foo\n";
        assert_eq!(
            find_kv(md, "next-target"),
            Some("investigate flaky test foo".into())
        );
    }

    #[test]
    fn kv_finds_key_in_bullet() {
        let md = "- baseline = 26.7m\n";
        assert_eq!(find_kv(md, "baseline"), Some("26.7m".into()));
    }

    #[test]
    fn kv_returns_none_when_missing() {
        assert_eq!(find_kv("nothing\n", "next-target"), None);
    }

    #[test]
    fn kv_handles_case_insensitive_key() {
        let md = "Next-Target: foo\n";
        assert_eq!(find_kv(md, "next-target"), Some("foo".into()));
    }
}