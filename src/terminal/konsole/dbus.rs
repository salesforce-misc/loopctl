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

//! zbus proxies for Konsole and KWin.
//!
//! Konsole exposes its API per-process: each running konsole shows up on
//! the session bus as `org.kde.konsole-<pid>`. There's no well-known
//! service name, so we always carry the destination explicitly.

use zbus::proxy;

#[proxy(interface = "org.kde.konsole.Window", default_path = "/Windows/1")]
pub trait KonsoleWindow {
    /// QStringList — Konsole returns session ids as decimal strings,
    /// e.g. `["1", "2"]`. Callers parse each into i32.
    #[zbus(name = "sessionList")]
    fn session_list(&self) -> zbus::Result<Vec<String>>;

    #[zbus(name = "currentSession")]
    fn current_session(&self) -> zbus::Result<i32>;

    #[zbus(name = "setCurrentSession")]
    fn set_current_session(&self, session_id: i32) -> zbus::Result<()>;

    /// Open a new tab using the named profile. Returns the new session id.
    #[zbus(name = "newSession")]
    fn new_session_with_profile(&self, profile: &str) -> zbus::Result<i32>;

    /// Open a new tab using the named profile, starting in `directory`.
    #[zbus(name = "newSession")]
    fn new_session_with_profile_dir(
        &self,
        profile: &str,
        directory: &str,
    ) -> zbus::Result<i32>;

    /// Open a new tab using the default profile.
    #[zbus(name = "newSession")]
    fn new_session_default(&self) -> zbus::Result<i32>;
}

#[proxy(interface = "org.kde.konsole.Session")]
pub trait KonsoleSession {
    #[zbus(name = "environment")]
    fn environment(&self) -> zbus::Result<Vec<String>>;

    #[zbus(name = "setEnvironment")]
    fn set_environment(&self, env: Vec<String>) -> zbus::Result<()>;

    #[zbus(name = "runCommand")]
    fn run_command(&self, command: &str) -> zbus::Result<()>;

    #[zbus(name = "sendText")]
    fn send_text(&self, text: &str) -> zbus::Result<()>;

    #[zbus(name = "setTitle")]
    fn set_title(&self, role: i32, title: &str) -> zbus::Result<()>;

    #[zbus(name = "title")]
    fn title(&self, role: i32) -> zbus::Result<String>;

    #[zbus(name = "processId")]
    fn process_id(&self) -> zbus::Result<i32>;

    #[zbus(name = "foregroundProcessId")]
    fn foreground_process_id(&self) -> zbus::Result<i32>;

    #[zbus(name = "close")]
    fn close(&self) -> zbus::Result<bool>;
}