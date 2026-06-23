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

// Re-exports the terminal plugin surface so integration tests in `tests/`
// and future modules (daemon, commands, tui) can call into it without going
// through a bin target.

pub mod config;
pub mod daemon;
pub mod discover;
pub mod dispatch;
pub mod metrics;
pub mod orchestrator;
pub mod readme;
pub mod repo_loader;
pub mod scheduler;
pub mod state;
pub mod terminal;
pub mod tui;
pub mod util;