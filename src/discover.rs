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

//! Walk a claude-loops repo and infer loop definitions.
//!
//! Per the user's confirmed approach: every `### <heading>` in
//! `<repo>/README.md` whose GFM-slugged anchor matches a top-level
//! directory under the repo (or a nested dir that exists) is a loop.
//! We don't try to parse beyond that — the body of the resume prompt
//! is owned by the README and stays in place.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::readme::slugify_heading;

/// One loop discovered in the repo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredLoop {
    /// Loop name. Derived from the dir path relative to the repo root,
    /// with `/` replaced by `-` (e.g. `example-project/perf` → name
    /// `example-project-perf`). Used as the lookup key in workbench.toml,
    /// state.json, and CLI args.
    pub name: String,
    /// Directory path relative to the repo root, `/` preserved.
    pub dir: String,
    /// GitHub-style anchor for the README heading.
    pub readme_anchor: String,
    /// The heading text itself (for human-readable previews / summary).
    pub heading: String,
    /// Suggested tab title — leaf component of the dir path.
    pub tab_title: String,
}

/// Discover loops in `repo_dir`. Reads `<repo_dir>/README.md` once,
/// walks H3 headings, and includes a heading whose anchor maps to an
/// existing dir in the repo.
///
/// Anchor → dir resolution rules (in order):
///   1. anchor `<a>--<b>--<c>` → try `<repo>/<a>/<b>/<c>` (joining `--` runs as path separators)
///   2. anchor `<a>--<b>` → try `<repo>/<a>` then `<repo>/<a>/<b>`
///   3. fall back to first path segment of the anchor (split on `--`).
pub fn discover_loops(repo_dir: &Path) -> Result<Vec<DiscoveredLoop>> {
    let readme = repo_dir.join("README.md");
    let body = std::fs::read_to_string(&readme)
        .with_context(|| format!("read {readme:?}"))?;

    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for line in body.lines() {
        let Some(rest) = line.strip_prefix("### ") else {
            continue;
        };
        let heading = rest.trim().to_string();
        if heading.is_empty() {
            continue;
        }
        let anchor = slugify_heading(&heading);
        let Some(dir_rel) = match_anchor_to_dir(repo_dir, &anchor) else {
            continue;
        };
        if !seen.insert(dir_rel.clone()) {
            continue;
        }
        let tab_title = leaf_title_from_dir(&dir_rel);
        out.push(DiscoveredLoop {
            name: dir_to_name(&dir_rel),
            dir: dir_rel,
            readme_anchor: anchor,
            heading,
            tab_title,
        });
    }
    Ok(out)
}

/// Try to resolve an anchor to an existing directory under `repo_dir`.
///
/// Splits the anchor on `--` (the GFM-slugged form of " — ") into parts.
/// For each (top, rest) split:
///
/// 1. If `repo_dir/<top>/<rest>` exists, return that.
/// 2. Else if `repo_dir/<top>` exists, scan its child dirs and pick
///    the one whose name appears as a substring inside `rest` (or vice
///    versa). E.g. heading "example-project — perf tuning" → anchor
///    "example-project--perf-tuning" → real dir `example-project/perf`.
fn match_anchor_to_dir(repo_dir: &Path, anchor: &str) -> Option<String> {
    let parts: Vec<&str> = anchor.split("--").filter(|s| !s.is_empty()).collect();
    if parts.is_empty() {
        return None;
    }
    // Longest-first exact match first.
    for end in (1..=parts.len()).rev() {
        let candidate = parts[..end].join("/");
        if repo_dir.join(&candidate).is_dir() {
            // If we matched only the first segment, prefer a deeper
            // match if a child dir's name appears in the rest.
            if end < parts.len() {
                let rest = parts[end..].join("-");
                if let Some(child) = pick_child_by_name(&repo_dir.join(&candidate), &rest) {
                    return Some(format!("{candidate}/{child}"));
                }
            }
            return Some(candidate);
        }
    }
    None
}

fn pick_child_by_name(parent: &Path, hint: &str) -> Option<String> {
    let entries = std::fs::read_dir(parent).ok()?;
    let mut best: Option<(String, usize)> = None;
    for entry in entries.flatten() {
        if !entry.file_type().ok()?.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') {
            continue;
        }
        // Match if either string is a substring of the other.
        if hint.contains(&name) || name.contains(hint) {
            // Score by name length so we prefer the longest matching
            // child (e.g. "java-vs-ejs-parity" beats "p" if both match).
            let score = name.len();
            best = match best {
                Some((_, prev)) if prev >= score => best,
                _ => Some((name, score)),
            };
        }
    }
    best.map(|(n, _)| n)
}

fn dir_to_name(rel_dir: &str) -> String {
    // Loop names in workbench.toml can't contain `/`. Replace with `-`.
    rel_dir.replace('/', "-")
}

fn leaf_title_from_dir(rel_dir: &str) -> String {
    rel_dir
        .rsplit('/')
        .next()
        .unwrap_or(rel_dir)
        .to_string()
}

/// Convenience wrapper for the CLI / TUI: returns a brief summary the
/// user can read before we write workbench.toml.
pub fn discover_summary(repo_dir: &Path) -> Result<DiscoverySummary> {
    let loops = discover_loops(repo_dir)?;
    Ok(DiscoverySummary {
        repo_dir: repo_dir.to_path_buf(),
        loops,
    })
}

#[derive(Debug, Clone)]
pub struct DiscoverySummary {
    pub repo_dir: PathBuf,
    pub loops: Vec<DiscoveredLoop>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_repo(root: &Path, headings_and_dirs: &[(&str, &str)], extra_lines: &[&str]) {
        std::fs::create_dir_all(root).unwrap();
        let mut readme = String::from("# repo\n\n");
        for (heading, dir) in headings_and_dirs {
            std::fs::create_dir_all(root.join(dir)).unwrap();
            readme.push_str(&format!("### {heading}\n\n```\nresume body\n```\n\n"));
        }
        for line in extra_lines {
            readme.push_str(line);
            readme.push('\n');
        }
        std::fs::write(root.join("README.md"), readme).unwrap();
    }

    #[test]
    fn finds_loops_for_top_level_dirs() {
        let dir = tempfile::tempdir().unwrap();
        write_repo(
            dir.path(),
            &[
                ("alpha-loop — example tracker", "alpha-loop"),
                ("beta-loop — example automation", "beta-loop"),
            ],
            &[],
        );
        let loops = discover_loops(dir.path()).unwrap();
        assert_eq!(loops.len(), 2);
        assert_eq!(loops[0].name, "alpha-loop");
        assert_eq!(loops[0].dir, "alpha-loop");
        assert_eq!(
            loops[0].readme_anchor,
            "alpha-loop--example-tracker"
        );
        assert_eq!(loops[1].name, "beta-loop");
    }

    #[test]
    fn finds_nested_loop_via_anchor_split() {
        let dir = tempfile::tempdir().unwrap();
        write_repo(
            dir.path(),
            &[(
                "example-project — Foo-bar perf tuning",
                "example-project/perf",
            )],
            &[],
        );
        let loops = discover_loops(dir.path()).unwrap();
        assert_eq!(loops.len(), 1);
        assert_eq!(loops[0].dir, "example-project/perf");
        assert_eq!(loops[0].name, "example-project-perf");
        assert_eq!(loops[0].tab_title, "perf");
    }

    #[test]
    fn skips_headings_without_matching_dirs() {
        let dir = tempfile::tempdir().unwrap();
        write_repo(
            dir.path(),
            &[("alpha-loop — example tracker", "alpha-loop")],
            &[
                "### How to resume after a restart",
                "",
                "```",
                "not a loop",
                "```",
                "",
            ],
        );
        let loops = discover_loops(dir.path()).unwrap();
        assert_eq!(loops.len(), 1);
        assert_eq!(loops[0].name, "alpha-loop");
    }

    #[test]
    fn dedupes_duplicate_headings_pointing_to_same_dir() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("alpha")).unwrap();
        let readme =
            "### alpha — first\n\n```\nx\n```\n\n### alpha — first\n\n```\nx\n```\n";
        std::fs::write(dir.path().join("README.md"), readme).unwrap();
        let loops = discover_loops(dir.path()).unwrap();
        assert_eq!(loops.len(), 1);
    }
}