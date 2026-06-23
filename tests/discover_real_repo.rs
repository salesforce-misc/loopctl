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

//! Integration test for `discover_loops` against a synthetic repo
//! constructed in a tempdir. Exercises the same surface area as a real
//! claude-loops repo: top-level loop dirs + a nested `<parent>/<child>`
//! loop reachable via the anchor-substring matcher.
//!
//! Used to be tied to the developer's live `~/claude-loops`; replaced
//! with a synthetic fixture for portability.

use agent_loop_workbench::discover::discover_loops;

#[test]
fn discovers_loops_in_synthetic_repo() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path();

    // Create the loop directories.
    for sub in [
        "alpha-loop",
        "beta-loop",
        "gamma-loop",
        "nested/inner-loop",
    ] {
        std::fs::create_dir_all(dir.join(sub)).unwrap();
    }

    // Write a README whose H3 anchors map to those dirs. The "nested"
    // case exercises the substring-fallback path in `match_anchor_to_dir`.
    let readme = "\
# Synthetic loops repo

## Active loops

| Loop | Project |
|------|---------|
| Alpha | alpha-loop |

## How to resume after a restart

Use the resume prompt below.

---

### alpha-loop — first loop

```
Resume the alpha loop.
```

### beta-loop — second loop

```
Resume the beta loop.
```

### gamma-loop — third loop

```
Resume the gamma loop.
```

### nested — inner loop

```
Resume the nested inner loop.
```
";
    std::fs::write(dir.join("README.md"), readme).unwrap();

    let loops = discover_loops(dir).expect("discover");
    let names: std::collections::HashSet<String> =
        loops.iter().map(|l| l.name.clone()).collect();
    for expected in [
        "alpha-loop",
        "beta-loop",
        "gamma-loop",
        "nested-inner-loop",
    ] {
        assert!(
            names.contains(expected),
            "expected name {expected} in {names:?}"
        );
    }
}