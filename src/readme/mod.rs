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

//! Extract per-loop resume prompts from `~/claude-loops/README.md`.
//!
//! Layout (the README owns this format; we don't):
//!
//! ```markdown
//! ### <some heading text>
//!
//! > Full context: [...](...)
//! ...
//!
//! ```
//! Resume the X loop. ...
//! ```
//! ```
//!
//! Each loop is identified by an `anchor` — the GFM-slugged form of the
//! heading text (lowercase, spaces → `-`, em-dash → `--`, punctuation
//! stripped). For a heading `example-project — Foo-bar perf tuning`,
//! GitHub's name is `example-project--foo-bar-perf-tuning`.
//!
//! `extract_resume_prompt` walks the README line by line, finds the H3
//! whose name matches, then returns the contents of the FIRST fenced code
//! block that follows (the resume prompt is always the first ``` block
//! after the heading; review/context paragraphs sit between in `>`-quoted
//! blocks).

use std::path::Path;

use anyhow::{anyhow, bail, Context, Result};

/// Convert a heading line (with leading `###` already stripped) to the
/// GitHub-flavored anchor name. Mirrors GitHub's algorithm closely enough
/// for our headings — in particular, em-dashes and ASCII hyphens both
/// collapse to one ASCII hyphen each, but interior whitespace becomes a
/// single hyphen, and surrounding punctuation is dropped.
pub fn slugify_heading(text: &str) -> String {
    // GitHub's algorithm (matches `jch-algoslug`):
    //   1. lowercase
    //   2. each space becomes a hyphen
    //   3. ASCII hyphens are kept; underscores are kept (we treat as separator)
    //   4. all other punctuation, em/en-dashes, emoji, etc. are stripped
    //   5. runs of hyphens are NOT collapsed
    //
    // For "example-project — Foo-bar perf tuning" the space + em-dash
    // + space becomes "--" (em-dash itself is dropped, two surrounding spaces
    // become two hyphens) which matches the in-README `[see below]` anchor.
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if c.is_alphanumeric() {
            for lc in c.to_lowercase() {
                out.push(lc);
            }
        } else if c == '-' || c == '_' || c.is_whitespace() {
            out.push('-');
        }
        // Em-dashes, en-dashes, periods, brackets, etc. are dropped entirely.
    }
    out
}

/// Extract the resume prompt for a given anchor from README.md text.
///
/// Returns the raw contents of the first fenced code block following the
/// matching `###` heading, with the surrounding ``` fences stripped. CRLF
/// is normalized to LF so callers can pass the result straight to
/// `TerminalPlugin::paste`.
pub fn extract_resume_prompt(readme: &str, anchor: &str) -> Result<String> {
    let mut lines = readme.lines().enumerate();

    // 1. Find the matching H3.
    let heading_idx = loop {
        let Some((i, line)) = lines.next() else {
            bail!("anchor `{anchor}` not found in README");
        };
        if let Some(rest) = line.strip_prefix("### ") {
            if slugify_heading(rest.trim()) == anchor {
                break i;
            }
        }
    };

    // 2. After the heading, find the first triple-backtick fence.
    let mut fence_lang_or_empty: Option<String> = None;
    let collected = readme
        .lines()
        .skip(heading_idx + 1)
        .scan((false, false), |(in_fence, done), line| {
            if *done {
                return None;
            }
            if !*in_fence {
                if let Some(rest) = line.strip_prefix("```") {
                    *in_fence = true;
                    fence_lang_or_empty = Some(rest.to_string());
                    Some(None) // skip the opening fence itself
                } else if line.starts_with("### ") || line.starts_with("## ") {
                    // Hit the next heading without finding a fence.
                    *done = true;
                    None
                } else {
                    Some(None)
                }
            } else if line.starts_with("```") {
                *done = true;
                None
            } else {
                Some(Some(line.to_string()))
            }
        })
        .flatten()
        .collect::<Vec<_>>();

    if fence_lang_or_empty.is_none() {
        bail!(
            "found heading for anchor `{anchor}` but no fenced code block before the next heading"
        );
    }
    let mut joined = collected.join("\n");
    joined = joined.replace("\r\n", "\n").replace('\r', "\n");
    Ok(joined)
}

/// Convenience: read `path` and extract.
pub fn extract_from_path(path: &Path, anchor: &str) -> Result<String> {
    let body =
        std::fs::read_to_string(path).with_context(|| format!("read README at {path:?}"))?;
    extract_resume_prompt(&body, anchor).map_err(|e| anyhow!("{e} (in {path:?})"))
}

/// Replace the fenced code block under the heading whose GFM-name matches
/// `anchor` with a new body. Returns the rewritten markdown WITHOUT
/// touching disk; callers that want to persist should use
/// [`replace_resume_prompt_in_file`].
pub fn replace_resume_prompt(readme: &str, anchor: &str, new_body: &str) -> Result<String> {
    let lines: Vec<String> = readme.lines().map(String::from).collect();
    // Find the matching H3.
    let heading_idx = lines
        .iter()
        .enumerate()
        .find_map(|(i, line)| {
            line.strip_prefix("### ")
                .filter(|rest| slugify_heading(rest.trim()) == anchor)
                .map(|_| i)
        })
        .ok_or_else(|| anyhow!("anchor `{anchor}` not found in README"))?;

    // Locate the opening fence after the heading (skip non-fence lines).
    let mut fence_open: Option<usize> = None;
    for (i, line) in lines.iter().enumerate().skip(heading_idx + 1) {
        if line.starts_with("### ") || line.starts_with("## ") {
            bail!(
                "found heading for `{anchor}` but next ``` block belongs to a later heading"
            );
        }
        if line.starts_with("```") {
            fence_open = Some(i);
            break;
        }
    }
    let open_idx = fence_open
        .ok_or_else(|| anyhow!("anchor `{anchor}` has no fenced code block"))?;
    let close_idx = lines
        .iter()
        .enumerate()
        .skip(open_idx + 1)
        .find_map(|(i, line)| line.starts_with("```").then_some(i))
        .ok_or_else(|| anyhow!("anchor `{anchor}` has an unclosed code fence"))?;

    // Replace lines (open_idx + 1 ..= close_idx - 1) with the new body's
    // lines. Opening + closing fence lines are preserved verbatim so the
    // user's language tag (e.g. "```bash") survives.
    let trimmed = new_body.trim_end_matches('\n');
    let new_body_lines: Vec<String> =
        if trimmed.is_empty() { Vec::new() } else { trimmed.lines().map(String::from).collect() };
    // Splice: keep [..=open_idx], insert new body, then keep [close_idx..].
    let mut spliced: Vec<String> = Vec::with_capacity(
        open_idx + 1 + new_body_lines.len() + (lines.len() - close_idx),
    );
    spliced.extend(lines[..=open_idx].iter().cloned());
    spliced.extend(new_body_lines);
    spliced.extend(lines[close_idx..].iter().cloned());
    let lines = spliced;

    let mut out = lines.join("\n");
    if !readme.ends_with('\n') {
        // Original had no trailing newline — preserve.
    } else if !out.ends_with('\n') {
        out.push('\n');
    }
    Ok(out)
}

/// Remove the entire H3 block under `anchor` from a README on disk —
/// the heading line, any preceding `---` separator immediately above
/// it, the body, and the fenced code block. Stops at the next H2/H3
/// or EOF.
///
/// Returns `Ok(true)` if a matching block was found and removed,
/// `Ok(false)` if the anchor wasn't present (caller decides whether
/// that's an error).
pub fn strip_readme_block_in_file(path: &Path, anchor: &str) -> Result<bool> {
    let body = std::fs::read_to_string(path)
        .with_context(|| format!("read README at {path:?}"))?;
    let Some(rewritten) = strip_readme_block(&body, anchor)? else {
        return Ok(false);
    };
    let tmp = path.with_extension("md.tmp");
    std::fs::write(&tmp, rewritten).with_context(|| format!("write {tmp:?}"))?;
    std::fs::rename(&tmp, path).with_context(|| format!("rename to {path:?}"))?;
    Ok(true)
}

/// Pure form of `strip_readme_block_in_file`. Returns `Ok(None)` when
/// the anchor isn't present so the caller can keep going.
pub fn strip_readme_block(readme: &str, anchor: &str) -> Result<Option<String>> {
    let lines: Vec<&str> = readme.lines().collect();
    let Some(heading_idx) = lines.iter().enumerate().find_map(|(i, line)| {
        line.strip_prefix("### ")
            .filter(|rest| slugify_heading(rest.trim()) == anchor)
            .map(|_| i)
    }) else {
        return Ok(None);
    };

    // Walk forward from the heading until we hit the next `### ` /
    // `## ` / `# ` heading, or EOF.
    let mut end_idx = lines.len();
    for (i, line) in lines.iter().enumerate().skip(heading_idx + 1) {
        if line.starts_with("### ") || line.starts_with("## ") || line.starts_with("# ") {
            end_idx = i;
            break;
        }
    }

    // Reach backward over a single trailing `---` separator (and the
    // blank line between it and the heading) so we leave a clean file.
    let mut start_idx = heading_idx;
    while start_idx > 0 && lines[start_idx - 1].trim().is_empty() {
        start_idx -= 1;
    }
    if start_idx > 0 && lines[start_idx - 1].trim() == "---" {
        start_idx -= 1;
    }

    // Likewise sweep blank lines AFTER the block so we don't leave a
    // double-blank gap.
    while end_idx < lines.len() && lines[end_idx].trim().is_empty() {
        end_idx += 1;
    }

    let mut out: Vec<&str> = Vec::with_capacity(lines.len());
    out.extend_from_slice(&lines[..start_idx]);
    out.extend_from_slice(&lines[end_idx..]);
    let mut joined = out.join("\n");
    if readme.ends_with('\n') && !joined.ends_with('\n') {
        joined.push('\n');
    }
    Ok(Some(joined))
}

/// Read `path`, replace the prompt under `anchor`, atomic rename back.
pub fn replace_resume_prompt_in_file(
    path: &Path,
    anchor: &str,
    new_body: &str,
) -> Result<()> {
    let body = std::fs::read_to_string(path)
        .with_context(|| format!("read README at {path:?}"))?;
    let rewritten = replace_resume_prompt(&body, anchor, new_body)?;
    let tmp = path.with_extension("md.tmp");
    std::fs::write(&tmp, rewritten).with_context(|| format!("write {tmp:?}"))?;
    std::fs::rename(&tmp, path).with_context(|| format!("rename to {path:?}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
# claude-loops

intro paragraph

## Active loops

| ... |

## How to resume

steps

---

### alpha-loop — Foo-bar perf tuning

> Full context: [...](...)
> — 6-phase plan, ...

```
Resume the alpha loop. Read ~/loops/...
for full context — line two.
Pull that file, orient yourself, then tell me
current phase and metric before asking what to do next.
```

---

### beta-loop — Issue tracker

> Full context: [...](...)

The loop is delta-aware: ...

```
Resume the beta tracker loop. Line one.

Then invoke the `beta-tracker` skill:
- Phase 1 (always): run ...
- Phase 2 (only if work.json non-empty): act on events ...

Commit tracker changes; auto-push.sh handles push.
```

---
";

    #[test]
    fn slug_matches_github_for_em_dash_with_spaces() {
        // Em-dash dropped, surrounding spaces → "--".
        let got = slugify_heading("alpha-loop — Foo-bar perf tuning");
        assert_eq!(got, "alpha-loop--foo-bar-perf-tuning");
    }

    #[test]
    fn slug_matches_github_for_simple_em_dash_separator() {
        let got = slugify_heading("beta-loop — Issue tracker");
        assert_eq!(got, "beta-loop--issue-tracker");
    }

    #[test]
    fn extracts_first_fenced_block_after_heading() {
        let got =
            extract_resume_prompt(SAMPLE, "alpha-loop--foo-bar-perf-tuning").unwrap();
        assert!(got.starts_with("Resume the alpha loop"));
        assert!(got.contains("current phase and metric"));
        // Must NOT include the closing fence
        assert!(!got.contains("```"));
        // Must NOT bleed into the next loop
        assert!(!got.contains("Resume the beta"));
    }

    #[test]
    fn extracts_with_blank_lines_preserved() {
        let got = extract_resume_prompt(SAMPLE, "beta-loop--issue-tracker").unwrap();
        assert!(got.starts_with("Resume the beta tracker loop"));
        assert!(got.contains("Phase 2 (only if work.json non-empty)"));
        // Embedded blank lines preserved
        assert!(got.contains("\n\nThen invoke"));
    }

    #[test]
    fn missing_anchor_errors() {
        let err = extract_resume_prompt(SAMPLE, "no-such-loop").unwrap_err();
        assert!(err.to_string().contains("not found"));
    }

    #[test]
    fn heading_without_fence_errors() {
        let bad = "### foo\n\nno code block here.\n\n### bar\n\n```\nbarprompt\n```\n";
        assert!(extract_resume_prompt(bad, "foo").is_err());
    }

    #[test]
    fn normalizes_crlf() {
        let body = "### x\n\n```\r\nline1\r\nline2\r\n```\n";
        let got = extract_resume_prompt(body, "x").unwrap();
        assert_eq!(got, "line1\nline2");
    }

    #[test]
    fn replace_swaps_only_the_target_block() {
        let got = replace_resume_prompt(
            SAMPLE,
            "beta-loop--issue-tracker",
            "BRAND NEW PROMPT\nline two",
        )
        .unwrap();
        // The new body landed.
        assert!(got.contains("BRAND NEW PROMPT"));
        assert!(got.contains("line two"));
        // The old body is gone.
        assert!(!got.contains("Phase 2 (only if work.json non-empty)"));
        // The OTHER loop's prompt is untouched.
        assert!(got.contains("Resume the alpha loop"));
        // Round-trip extraction returns the new body verbatim.
        let extracted =
            extract_resume_prompt(&got, "beta-loop--issue-tracker").unwrap();
        assert_eq!(extracted, "BRAND NEW PROMPT\nline two");
    }

    #[test]
    fn replace_preserves_fence_language() {
        let body = "### x\n\n```bash\nold\n```\n";
        let got = replace_resume_prompt(body, "x", "new").unwrap();
        assert!(got.contains("```bash"));
        assert!(got.contains("\nnew\n"));
        assert!(!got.contains("old"));
    }

    #[test]
    fn replace_with_empty_body_yields_empty_block() {
        let body = "### x\n\n```\nold\n```\n";
        let got = replace_resume_prompt(body, "x", "").unwrap();
        // Two adjacent fences with nothing between them.
        assert!(got.contains("```\n```"));
    }

    #[test]
    fn replace_missing_anchor_errors() {
        let err =
            replace_resume_prompt(SAMPLE, "nope", "x").unwrap_err().to_string();
        assert!(err.contains("not found"));
    }

    #[test]
    fn strip_removes_block_and_separator() {
        // SAMPLE has alpha-loop and beta-loop. Strip alpha-loop and
        // make sure beta-loop survives.
        let out = strip_readme_block(SAMPLE, "alpha-loop--foo-bar-perf-tuning")
            .unwrap()
            .expect("alpha block must exist");
        assert!(!out.contains("Resume the alpha loop"));
        assert!(!out.contains("alpha-loop — Foo-bar perf tuning"));
        // The other loop's block is intact.
        assert!(out.contains("Resume the beta tracker loop"));
        assert!(out.contains("beta-loop — Issue tracker"));
        // The leading `---` separator immediately above the alpha
        // heading should have been swept too.
        let alpha_count = out.matches("alpha").count();
        // (allow 0; a hint elsewhere in SAMPLE could mention it but
        // SAMPLE doesn't.)
        assert_eq!(alpha_count, 0, "alpha references survived: {out}");
    }

    #[test]
    fn strip_returns_none_for_missing_anchor() {
        let out = strip_readme_block(SAMPLE, "no-such-anchor").unwrap();
        assert!(out.is_none());
    }

    #[test]
    fn strip_handles_eof_block() {
        // Block at the very end of the file (no trailing heading).
        let body = "## intro\n\nstuff\n\n### tail-loop — last\n\n```\nresume tail\n```\n";
        let out = strip_readme_block(body, "tail-loop--last")
            .unwrap()
            .expect("tail block must exist");
        assert!(!out.contains("tail-loop"));
        assert!(out.contains("## intro"));
    }
}