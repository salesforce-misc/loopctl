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

//! Shared helpers — XDG paths and tilde expansion.

pub mod paths;

/// Expand `~` and `$VAR` in a path string. Falls back to the raw input on
/// error so config validation can produce a more helpful message later.
pub fn expand_path(s: &str) -> String {
    shellexpand::full(s)
        .map(|cow| cow.into_owned())
        .unwrap_or_else(|_| s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expand_tilde() {
        let home = std::env::var("HOME").unwrap();
        assert_eq!(expand_path("~/foo"), format!("{home}/foo"));
    }

    #[test]
    fn expand_passes_through_absolute() {
        assert_eq!(expand_path("/etc/passwd"), "/etc/passwd");
    }
}