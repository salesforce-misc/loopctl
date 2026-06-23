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

//! Cargo-test wrapper that runs the BATS shell-level CLI suites.
//!
//! On first run, downloads + extracts a pinned BATS release into
//! `tests/cli/.bats-cache/` (gitignored). Then invokes `bats` against
//! every `*.bats` file under `tests/cli/`.
//!
//! Set `LOOPCTL_SKIP_BATS=1` to skip — useful when a CI runner is
//! offline and `BATS_OFFLINE_TARBALL` isn't pre-staged.

use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn ensure_loopctl_built() -> PathBuf {
    let bin = manifest_dir().join("target/debug/loopctl");
    if !bin.exists() {
        let status = Command::new("cargo")
            .args(["build", "--bin", "loopctl"])
            .current_dir(manifest_dir())
            .status()
            .expect("cargo build --bin loopctl");
        assert!(status.success(), "cargo build --bin loopctl failed");
    }
    bin
}

#[test]
fn bats_cli_suites() {
    if std::env::var_os("LOOPCTL_SKIP_BATS").is_some() {
        eprintln!("LOOPCTL_SKIP_BATS set — skipping BATS suites");
        return;
    }

    // Ensure the binary the BATS suites exercise is up to date.
    ensure_loopctl_built();

    // Bootstrap (or reuse cached) BATS install.
    let bootstrap = manifest_dir().join("tests/cli/bootstrap-bats.sh");
    let bootstrap_out = Command::new(&bootstrap)
        .output()
        .unwrap_or_else(|e| panic!("run {bootstrap:?}: {e}"));
    if !bootstrap_out.status.success() {
        panic!(
            "BATS bootstrap failed (status {:?}):\nstdout: {}\nstderr: {}",
            bootstrap_out.status.code(),
            String::from_utf8_lossy(&bootstrap_out.stdout),
            String::from_utf8_lossy(&bootstrap_out.stderr),
        );
    }
    let bats_path = String::from_utf8_lossy(&bootstrap_out.stdout)
        .trim()
        .to_string();
    assert!(
        std::path::Path::new(&bats_path).is_file(),
        "bootstrap printed a non-file BATS path: {bats_path}"
    );

    // Find every *.bats file under tests/cli/. Glob deliberately not
    // recursive — the cache directory contains BATS itself which has
    // its own *.bats fixtures we must not run.
    let cli_dir = manifest_dir().join("tests/cli");
    let mut bats_files: Vec<PathBuf> = std::fs::read_dir(&cli_dir)
        .expect("read tests/cli")
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_file())
        .filter(|e| e.path().extension().map(|s| s == "bats").unwrap_or(false))
        .map(|e| e.path())
        .collect();
    bats_files.sort();
    assert!(!bats_files.is_empty(), "no *.bats files in {cli_dir:?}");

    let status = Command::new(&bats_path)
        .args(&bats_files)
        .status()
        .expect("invoke bats");
    assert!(status.success(), "bats reported failures (see output above)");
}