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

//! Build script that emits `LOOPCTL_VERSION` for the daemon and CLI to
//! print at runtime. The string is `<crate-version>-<git-sha>[-dirty]`
//! when the build runs inside a git checkout, falling back to the bare
//! crate version when git is unavailable (e.g. cargo-published source).
//!
//! Triggers a rebuild when HEAD or the index changes so a fresh commit
//! produces a fresh version string.

use std::process::Command;

fn main() {
    let crate_version = env!("CARGO_PKG_VERSION");
    let version = match git_describe() {
        Some(v) => format!("{crate_version}-{v}"),
        None => crate_version.to_string(),
    };
    println!("cargo:rustc-env=LOOPCTL_VERSION={version}");

    // Re-run when HEAD moves so a fresh commit refreshes the version.
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/index");
}

/// Returns the short SHA, with a `-dirty` suffix if the working tree
/// has uncommitted changes. Returns None if git is unavailable or the
/// build is happening outside a checkout.
fn git_describe() -> Option<String> {
    let sha_out = Command::new("git")
        .args(["rev-parse", "--short=12", "HEAD"])
        .output()
        .ok()?;
    if !sha_out.status.success() {
        return None;
    }
    let sha = String::from_utf8(sha_out.stdout).ok()?;
    let sha = sha.trim();
    if sha.is_empty() {
        return None;
    }

    let dirty_out = Command::new("git")
        .args(["status", "--porcelain"])
        .output()
        .ok()?;
    let dirty = dirty_out.status.success() && !dirty_out.stdout.is_empty();
    Some(if dirty {
        format!("{sha}-dirty")
    } else {
        sha.to_string()
    })
}