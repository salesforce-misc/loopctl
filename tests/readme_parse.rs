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

//! Golden tests for `readme::extract_resume_prompt` against a synthetic
//! README built in a tempdir. Used to point at a developer-local
//! claude-loops repo; replaced for portability + so tests run anywhere
//! without depending on private content.

use std::path::PathBuf;

use agent_loop_workbench::readme::extract_from_path;

const SYNTHETIC_README: &str = "\
# Synthetic loops repo

## Active loops

| Loop |
|------|
| Alpha |

---

### alpha-loop — first loop

```
Resume the alpha loop. Read alpha-loop/progress.md
for full context.
```

### beta-loop — second loop

```
Resume the beta loop. Read beta-loop/progress.md
for full context.
```

### nested — inner loop

```
Resume the nested inner loop.
```
";

fn write_synthetic_readme() -> PathBuf {
    let dir = tempfile::tempdir().expect("tempdir").keep();
    let path = dir.join("README.md");
    std::fs::write(&path, SYNTHETIC_README).unwrap();
    path
}

macro_rules! anchor_test {
    ($name:ident, $anchor:expr, $needle:expr) => {
        #[test]
        fn $name() {
            let path = write_synthetic_readme();
            let got =
                extract_from_path(&path, $anchor).expect("extract resume prompt");
            assert!(
                !got.trim().is_empty(),
                "extracted prompt was empty for anchor {}",
                $anchor
            );
            assert!(
                !got.contains("```"),
                "extracted prompt for {} still contains backticks",
                $anchor
            );
            assert!(
                got.contains($needle),
                "extracted prompt for {} did not contain expected substring {:?}\n--- got ---\n{}",
                $anchor,
                $needle,
                got
            );
        }
    };
}

anchor_test!(
    alpha_loop,
    "alpha-loop--first-loop",
    "Resume the alpha loop"
);
anchor_test!(
    beta_loop,
    "beta-loop--second-loop",
    "Resume the beta loop"
);
anchor_test!(
    nested_inner,
    "nested--inner-loop",
    "Resume the nested inner loop"
);