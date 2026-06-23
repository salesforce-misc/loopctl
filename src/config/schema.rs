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

//! Pure-data structs for `workbench.toml`. No I/O.
//!
//! The on-disk shape:
//!
//! ```toml
//! schema_version = 1
//!
//! [workbench]
//! claude_loops_dir = "~/claude-loops"
//! terminal         = "konsole"
//! central_group    = "1"
//! claude_binary    = "claude"
//! default_dispatch = "resume_prompt"
//!
//! [terminal.konsole]
//! profile = "ClaudeLoop"  # optional
//!
//! [[loops]]
//! name             = "alpha-loop"
//! dir              = "alpha-loop"
//! readme_anchor    = "alpha-loop--example-tracker"
//! group_key        = "2"
//! tab_title        = "alpha-loop"      # defaults to `name`
//! schedule         = "every 10m"        # or "none", "daily 09:00 America/Los_Angeles", ...
//! dispatch         = "tick_script"
//! tick_script      = "skill/scripts/tick.py"  # required iff dispatch = "tick_script"
//! ```

use std::str::FromStr;

use anyhow::{anyhow, bail, Result};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkbenchConfig {
    pub schema_version: u32,
    /// Bumped by one-shot migrations applied at daemon startup. Absent
    /// or zero ⇒ no migrations have run yet on this file.
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub loopctl_migration_v: u32,
    pub workbench: WorkbenchSettings,
    #[serde(default)]
    pub terminal: TerminalSettings,
    #[serde(default)]
    pub loops: Vec<LoopConfig>,
}

fn is_zero_u32(n: &u32) -> bool {
    *n == 0
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkbenchSettings {
    pub claude_loops_dir: String,
    /// Plugin name. Currently accepts `konsole` (KDE/X11) or
    /// `iterm2` (macOS). Other backends (tmux, mac-terminal) are
    /// stubs and are rejected at validation time.
    pub terminal: String,
    pub central_group: String,
    #[serde(default = "default_claude_binary")]
    pub claude_binary: String,
    #[serde(default = "default_dispatch_kind")]
    pub default_dispatch: Dispatch,
    /// Default args appended to every `claude` invocation. Loops with
    /// `claude_args = []` (an empty array) explicitly opt out and get
    /// stock claude with normal permission prompting. Loops that omit
    /// the field entirely inherit this list.
    #[serde(default = "default_claude_args")]
    pub claude_args: Vec<String>,
    /// Milliseconds to wait between pasting the resume prompt body and
    /// sending the CR (Enter) that submits it. Default 700ms — long
    /// enough for claude to finish startup and enable raw mode, but
    /// short enough that you don't sit and stare at a tab. Bump this
    /// if you see prompts not auto-submitting (manual Enter required);
    /// drop it if you see your prompt body get partially submitted.
    #[serde(default = "default_paste_submit_delay_ms")]
    pub paste_submit_delay_ms: u64,
}

fn default_paste_submit_delay_ms() -> u64 {
    700
}

fn default_claude_binary() -> String {
    "claude".to_string()
}
fn default_dispatch_kind() -> Dispatch {
    Dispatch::ResumePrompt
}
fn default_claude_args() -> Vec<String> {
    // Long-running loops are trusted to run hands-off — see README.
    // Override at workbench level by setting `claude_args = [...]` or
    // disable per-loop by setting `claude_args = []`.
    vec!["--dangerously-skip-permissions".to_string()]
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TerminalSettings {
    #[serde(default)]
    pub konsole: KonsoleSettings,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct KonsoleSettings {
    /// Konsole profile name. `None` ⇒ use the user's default profile.
    pub profile: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoopConfig {
    /// Unique loop identifier. Used as the lookup key everywhere
    /// (state.json, RPC params, CLI args, TUI selection). Older
    /// configs called this field `slug`; the alias keeps them parsing.
    /// Daemon migration v2 rewrites `slug` to `name` on disk.
    #[serde(alias = "slug")]
    pub name: String,
    pub dir: String,
    pub readme_anchor: String,
    pub group_key: String,
    /// Defaults to `name` if omitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tab_title: Option<String>,
    /// Cron-ish schedule. Parsed at validation time.
    pub schedule: String,
    pub dispatch: Dispatch,
    /// Required iff `dispatch == TickScript`. Path relative to `dir`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tick_script: Option<String>,
    /// Optional args appended to `python3 <tick_script>`. Each element is
    /// passed as a single argv entry; shell expansion happens here for
    /// `~` and `$VAR` so users can write `--out "$HOME/.claude/foo.json"`.
    /// Defaults to no args.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tick_args: Vec<String>,
    /// Per-loop claude args. `None` ⇒ inherit `WorkbenchSettings.claude_args`.
    /// `Some(vec![])` ⇒ explicit opt-out: run stock `claude` with the
    /// user's normal permission prompts (the typical state for a brand
    /// new loop that hasn't earned hands-off trust yet).
    ///
    /// `skip_serializing_if = is_none` is critical: without it, every
    /// toml round-trip (move-group, close-loop, etc.) emits
    /// `claude_args = []` for `None`, which silently overrides the
    /// workbench-wide default the next time the config is read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claude_args: Option<Vec<String>>,
    /// `false` ⇒ this loop is closed: skip it during `up`, scheduled
    /// ticks, and health checks; close any live pane. Persists across
    /// daemon restarts. Defaults to `true` (active) when the field is
    /// absent so older configs keep working.
    #[serde(default = "default_true")]
    pub active: bool,
}

fn default_true() -> bool {
    true
}

impl LoopConfig {
    /// Tab title, or fall back to `name`.
    pub fn effective_tab_title(&self) -> &str {
        self.tab_title.as_deref().unwrap_or(&self.name)
    }

    /// Parse the `schedule` field eagerly.
    pub fn parsed_schedule(&self) -> Result<Schedule> {
        Schedule::from_str(&self.schedule)
    }

    /// Effective claude args for this loop. If the loop has its own
    /// `claude_args` set (even an empty list), it wins. Otherwise the
    /// workbench-wide default applies.
    pub fn effective_claude_args<'a>(
        &'a self,
        workbench: &'a WorkbenchSettings,
    ) -> &'a [String] {
        self.claude_args
            .as_deref()
            .unwrap_or(&workbench.claude_args)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Dispatch {
    /// Re-paste the loop's resume prompt into its pane.
    ResumePrompt,
    /// Spawn `tick_script` as a subprocess (cwd = loop dir).
    TickScript,
    /// No automatic dispatch — only manual `loopctl tick <loop>` works.
    None,
}

/// Hand-rolled cron-ish schedule. Owns a small number of variants on
/// purpose — keeps parsing simple and surface aimed at our actual use cases.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Schedule {
    /// `none` ⇒ never auto-fire.
    Never,
    /// `every Nm` ⇒ fire every N minutes.
    EveryMinutes(u32),
    /// `daily HH:MM <Tz>` ⇒ fire once per day at HH:MM in the named tz.
    DailyAt { hh: u8, mm: u8, tz: Tz },
    /// `weekly Mon HH:MM <Tz>` ⇒ fire weekly.
    WeeklyAt {
        weekday: chrono::Weekday,
        hh: u8,
        mm: u8,
        tz: Tz,
    },
}

impl FromStr for Schedule {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self> {
        let s = s.trim();
        if s.eq_ignore_ascii_case("none") || s.is_empty() {
            return Ok(Self::Never);
        }

        // every Nm
        if let Some(rest) = s.strip_prefix("every ").or_else(|| s.strip_prefix("every\t")) {
            let rest = rest.trim();
            let mins = parse_minutes(rest)
                .ok_or_else(|| anyhow!("could not parse `every {rest}` as minutes"))?;
            if mins == 0 {
                bail!("`every` interval must be > 0");
            }
            return Ok(Self::EveryMinutes(mins));
        }

        // daily HH:MM <Tz>
        if let Some(rest) = s.strip_prefix("daily ") {
            let mut parts = rest.split_whitespace();
            let hhmm = parts
                .next()
                .ok_or_else(|| anyhow!("daily: missing HH:MM"))?;
            let tz_str = parts
                .next()
                .ok_or_else(|| anyhow!("daily: missing timezone"))?;
            if parts.next().is_some() {
                bail!("daily: too many fields in `{s}`");
            }
            let (hh, mm) = parse_hhmm(hhmm)?;
            let tz: Tz = tz_str
                .parse()
                .map_err(|e| anyhow!("daily: invalid timezone `{tz_str}`: {e}"))?;
            return Ok(Self::DailyAt { hh, mm, tz });
        }

        // weekly Day HH:MM <Tz>
        if let Some(rest) = s.strip_prefix("weekly ") {
            let mut parts = rest.split_whitespace();
            let day_str = parts
                .next()
                .ok_or_else(|| anyhow!("weekly: missing weekday"))?;
            let hhmm = parts
                .next()
                .ok_or_else(|| anyhow!("weekly: missing HH:MM"))?;
            let tz_str = parts
                .next()
                .ok_or_else(|| anyhow!("weekly: missing timezone"))?;
            if parts.next().is_some() {
                bail!("weekly: too many fields in `{s}`");
            }
            let weekday = parse_weekday(day_str)?;
            let (hh, mm) = parse_hhmm(hhmm)?;
            let tz: Tz = tz_str
                .parse()
                .map_err(|e| anyhow!("weekly: invalid timezone `{tz_str}`: {e}"))?;
            return Ok(Self::WeeklyAt {
                weekday,
                hh,
                mm,
                tz,
            });
        }

        bail!(
            "unrecognized schedule `{s}`; supported: \
             `none`, `every Nm`, `daily HH:MM <Tz>`, `weekly <Day> HH:MM <Tz>`"
        )
    }
}

fn parse_minutes(s: &str) -> Option<u32> {
    let stripped = s.strip_suffix('m').or_else(|| s.strip_suffix("min")).unwrap_or(s);
    stripped.parse().ok()
}

fn parse_hhmm(s: &str) -> Result<(u8, u8)> {
    let (h, m) = s
        .split_once(':')
        .ok_or_else(|| anyhow!("expected HH:MM, got `{s}`"))?;
    let hh: u8 = h.parse().map_err(|e| anyhow!("bad HH `{h}`: {e}"))?;
    let mm: u8 = m.parse().map_err(|e| anyhow!("bad MM `{m}`: {e}"))?;
    if hh >= 24 || mm >= 60 {
        bail!("HH:MM out of range: {hh}:{mm:02}");
    }
    Ok((hh, mm))
}

fn parse_weekday(s: &str) -> Result<chrono::Weekday> {
    use chrono::Weekday::*;
    Ok(match s.to_ascii_lowercase().as_str() {
        "mon" | "monday" => Mon,
        "tue" | "tues" | "tuesday" => Tue,
        "wed" | "weds" | "wednesday" => Wed,
        "thu" | "thur" | "thurs" | "thursday" => Thu,
        "fri" | "friday" => Fri,
        "sat" | "saturday" => Sat,
        "sun" | "sunday" => Sun,
        other => bail!("unknown weekday `{other}`"),
    })
}

/// Returns true iff `s` is a single non-empty token of `[A-Za-z0-9._-]`.
/// Used to lock down `group_key` and `tab_title` because some terminal
/// backends (notably iTerm2 via AppleScript) embed these into shell or
/// scripting strings with only `"`-quoting. Validating at config-parse
/// time blocks the whole class of injection at the door instead of
/// auditing every backend's escaping.
fn is_safe_identifier(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// Returns true iff `path` is a relative path with no `..` components.
/// Absolute paths and any segment equal to `..` are rejected — these
/// would let a config typo (or hostile config) escape the
/// `claude_loops_dir` sandbox.
fn is_safe_relative_path(path: &str) -> bool {
    let p = std::path::Path::new(path);
    if p.is_absolute() {
        return false;
    }
    use std::path::Component;
    p.components().all(|c| match c {
        Component::Normal(_) | Component::CurDir => true,
        Component::ParentDir | Component::RootDir | Component::Prefix(_) => false,
    })
}

impl WorkbenchConfig {
    pub fn validate(&self) -> Result<()> {
        if self.schema_version != 1 {
            bail!(
                "unsupported schema_version {} (expected 1)",
                self.schema_version
            );
        }
        if !matches!(self.workbench.terminal.as_str(), "konsole" | "iterm2") {
            bail!(
                "workbench.terminal = `{}` not supported (valid: `konsole`, `iterm2`)",
                self.workbench.terminal
            );
        }
        // Loop names must be unique — they're the lookup key in
        // state.json, RPC params, and CLI subcommands. A duplicate
        // means `loopctl tick <name>` (etc.) becomes ambiguous.
        let mut seen: std::collections::HashMap<&str, usize> =
            std::collections::HashMap::new();
        for (i, lc) in self.loops.iter().enumerate() {
            if let Some(prev_i) = seen.insert(lc.name.as_str(), i) {
                bail!(
                    "duplicate loop name `{}` (already declared at [[loops]] #{prev}; this is at #{cur}). \
                     Loop names must be unique — they're the key for state, RPC, and CLI lookups.",
                    lc.name,
                    prev = prev_i + 1,
                    cur = i + 1,
                );
            }
            if lc.name.trim().is_empty() {
                bail!("loop at [[loops]] #{} has an empty name", i + 1);
            }
            // Path-traversal defense: `dir` is joined under
            // `claude_loops_dir` and `tick_script` is joined under
            // `dir`. Both must stay inside the sandbox; reject `..`
            // and absolute paths.
            if !is_safe_relative_path(&lc.dir) {
                bail!(
                    "loop `{}`: dir = `{}` must be a relative path with no `..` components",
                    lc.name,
                    lc.dir
                );
            }
            if let Some(ts) = lc.tick_script.as_deref() {
                if !is_safe_relative_path(ts) {
                    bail!(
                        "loop `{}`: tick_script = `{}` must be a relative path with no `..` components",
                        lc.name,
                        ts
                    );
                }
            }
            // Charset whitelist for `group_key` and `tab_title`. Some
            // terminal backends interpolate these into AppleScript /
            // shell strings; constraining them at config time stops
            // injection regardless of backend escaping.
            if !is_safe_identifier(&lc.group_key) {
                bail!(
                    "loop `{}`: group_key = `{}` must match [A-Za-z0-9._-]+",
                    lc.name,
                    lc.group_key
                );
            }
            if let Some(t) = lc.tab_title.as_deref() {
                if !is_safe_identifier(t) {
                    bail!(
                        "loop `{}`: tab_title = `{}` must match [A-Za-z0-9._-]+",
                        lc.name,
                        t
                    );
                }
            }
            // Eager schedule parse.
            lc.parsed_schedule()
                .map_err(|e| anyhow!("loop `{}`: invalid schedule: {e}", lc.name))?;
            match (lc.dispatch, lc.tick_script.as_ref()) {
                (Dispatch::TickScript, None) => bail!(
                    "loop `{}`: dispatch = `tick_script` requires a `tick_script` field",
                    lc.name
                ),
                (Dispatch::TickScript, Some(_)) => {}
                (_, Some(_)) => bail!(
                    "loop `{}`: tick_script is set but dispatch != tick_script",
                    lc.name
                ),
                (_, None) => {}
            }
        }
        Ok(())
    }

    pub fn loop_by_name(&self, name: &str) -> Option<&LoopConfig> {
        self.loops.iter().find(|l| l.name == name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Weekday;

    fn parse(body: &str) -> Result<WorkbenchConfig> {
        let cfg: WorkbenchConfig = toml::from_str(body)?;
        cfg.validate()?;
        Ok(cfg)
    }

    const MINIMAL: &str = r#"
schema_version = 1

[workbench]
claude_loops_dir = "~/claude-loops"
terminal = "konsole"
central_group = "1"

[[loops]]
name = "alpha-loop"
dir = "alpha-loop"
readme_anchor = "alpha-loop--example-tracker"
group_key = "2"
schedule = "every 10m"
dispatch = "tick_script"
tick_script = "skill/scripts/tick.py"
"#;

    #[test]
    fn parses_minimal_config() {
        let cfg = parse(MINIMAL).expect("parse");
        assert_eq!(cfg.schema_version, 1);
        assert_eq!(cfg.workbench.terminal, "konsole");
        assert_eq!(cfg.workbench.claude_binary, "claude"); // default
        assert_eq!(cfg.workbench.default_dispatch, Dispatch::ResumePrompt); // default
        assert_eq!(cfg.loops.len(), 1);
        let lc = &cfg.loops[0];
        assert_eq!(lc.effective_tab_title(), "alpha-loop");
        assert_eq!(lc.parsed_schedule().unwrap(), Schedule::EveryMinutes(10));
    }

    #[test]
    fn legacy_slug_field_still_parses_via_alias() {
        // Older configs use `slug = "..."`. The serde alias keeps them
        // working; the daemon's migration v2 rewrites them to `name`.
        let body = MINIMAL.replace("name = \"alpha-loop\"", "slug = \"alpha-loop\"");
        let cfg = parse(&body).expect("parse with legacy slug");
        assert_eq!(cfg.loops[0].name, "alpha-loop");
    }

    #[test]
    fn rejects_unknown_terminal() {
        let body = MINIMAL.replace(r#"terminal = "konsole""#, r#"terminal = "tmux""#);
        let err = parse(&body).unwrap_err().to_string();
        assert!(err.contains("not supported"));
    }

    #[test]
    fn accepts_iterm2_terminal() {
        let body = MINIMAL.replace(r#"terminal = "konsole""#, r#"terminal = "iterm2""#);
        parse(&body).expect("iterm2 should be valid");
    }

    #[test]
    fn rejects_duplicate_name_with_clear_error() {
        let body = format!(
            "{MINIMAL}\n[[loops]]\nname = \"alpha-loop\"\ndir = \"alpha-loop\"\n\
             readme_anchor = \"x\"\ngroup_key = \"2\"\nschedule = \"none\"\n\
             dispatch = \"none\"\n"
        );
        let err = parse(&body).unwrap_err().to_string();
        assert!(err.contains("duplicate loop name"), "got: {err}");
        assert!(err.contains("alpha-loop"));
        assert!(err.contains("must be unique"));
    }

    #[test]
    fn rejects_duplicate_name_when_one_uses_legacy_slug_alias() {
        // Mixing `slug = "x"` and `name = "x"` should still fail uniqueness.
        let body = format!(
            "{MINIMAL}\n[[loops]]\nslug = \"alpha-loop\"\ndir = \"alpha-loop\"\n\
             readme_anchor = \"x\"\ngroup_key = \"2\"\nschedule = \"none\"\n\
             dispatch = \"none\"\n"
        );
        let err = parse(&body).unwrap_err().to_string();
        assert!(err.contains("duplicate loop name"), "got: {err}");
    }

    #[test]
    fn rejects_empty_name() {
        let body = MINIMAL.replace("name = \"alpha-loop\"", "name = \"\"");
        let err = parse(&body).unwrap_err().to_string();
        assert!(err.contains("empty name"), "got: {err}");
    }

    #[test]
    fn tick_script_required_for_tick_script_dispatch() {
        let body = MINIMAL.replace(
            "tick_script = \"skill/scripts/tick.py\"",
            "",
        );
        let err = parse(&body).unwrap_err().to_string();
        assert!(err.contains("requires a `tick_script` field"));
    }

    #[test]
    fn tick_script_forbidden_outside_tick_script_dispatch() {
        let body = MINIMAL
            .replace("dispatch = \"tick_script\"", "dispatch = \"resume_prompt\"")
            .to_string();
        // tick_script left in place — should fail validation
        let err = parse(&body).unwrap_err().to_string();
        assert!(err.contains("tick_script is set but dispatch != tick_script"));
    }

    #[test]
    fn schedule_parses_every_minutes() {
        assert_eq!(
            "every 10m".parse::<Schedule>().unwrap(),
            Schedule::EveryMinutes(10)
        );
        assert_eq!(
            "every 60min".parse::<Schedule>().unwrap(),
            Schedule::EveryMinutes(60)
        );
        assert!("every 0m".parse::<Schedule>().is_err());
    }

    #[test]
    fn schedule_parses_daily() {
        let s: Schedule = "daily 09:00 America/Los_Angeles".parse().unwrap();
        match s {
            Schedule::DailyAt { hh, mm, tz } => {
                assert_eq!((hh, mm), (9, 0));
                assert_eq!(tz, chrono_tz::America::Los_Angeles);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn schedule_parses_weekly() {
        let s: Schedule = "weekly Mon 08:00 America/Los_Angeles".parse().unwrap();
        match s {
            Schedule::WeeklyAt {
                weekday,
                hh,
                mm,
                tz,
            } => {
                assert_eq!(weekday, Weekday::Mon);
                assert_eq!((hh, mm), (8, 0));
                assert_eq!(tz, chrono_tz::America::Los_Angeles);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn schedule_parses_none() {
        assert_eq!("none".parse::<Schedule>().unwrap(), Schedule::Never);
        assert_eq!("".parse::<Schedule>().unwrap(), Schedule::Never);
    }

    #[test]
    fn schedule_rejects_garbage() {
        assert!("daily 25:00 UTC".parse::<Schedule>().is_err());
        assert!("daily 09:00".parse::<Schedule>().is_err());
        assert!("weekly Funday 09:00 UTC".parse::<Schedule>().is_err());
        assert!("hourly".parse::<Schedule>().is_err());
    }

    #[test]
    fn rejects_unsupported_schema_version() {
        let body = MINIMAL.replace("schema_version = 1", "schema_version = 2");
        let err = parse(&body).unwrap_err().to_string();
        assert!(err.contains("unsupported schema_version 2"));
    }

    #[test]
    fn round_trip_omits_none_claude_args() {
        // Loop with claude_args left absent must NOT serialize as
        // `claude_args = []` — that empty array would silently override
        // the workbench-wide default the next time the file is loaded.
        let cfg = parse(MINIMAL).unwrap();
        assert!(cfg.loops[0].claude_args.is_none());
        let body = toml::to_string_pretty(&cfg).unwrap();
        // The workbench-wide `claude_args` IS allowed; only the per-loop
        // one should be missing.
        let after_loops = body
            .split_once("[[loops]]")
            .map(|(_, rest)| rest)
            .unwrap_or(&body);
        assert!(
            !after_loops.contains("claude_args"),
            "round-trip leaked per-loop claude_args:\n{body}"
        );
    }

    #[test]
    fn round_trip_preserves_explicit_empty_claude_args() {
        let body = MINIMAL.replace(
            "tick_script = \"skill/scripts/tick.py\"",
            "tick_script = \"skill/scripts/tick.py\"\nclaude_args = []",
        );
        let cfg = parse(&body).unwrap();
        assert_eq!(cfg.loops[0].claude_args.as_deref(), Some(&[][..]));
        let out = toml::to_string_pretty(&cfg).unwrap();
        assert!(out.contains("claude_args = []"));
    }

    #[test]
    fn rejects_dir_with_parent_traversal() {
        let body = MINIMAL.replace(r#"dir = "alpha-loop""#, r#"dir = "../escape""#);
        let err = parse(&body).unwrap_err().to_string();
        assert!(err.contains("must be a relative path"), "got: {err}");
    }

    #[test]
    fn rejects_absolute_dir() {
        let body = MINIMAL.replace(r#"dir = "alpha-loop""#, r#"dir = "/etc/passwd""#);
        let err = parse(&body).unwrap_err().to_string();
        assert!(err.contains("must be a relative path"), "got: {err}");
    }

    #[test]
    fn accepts_nested_relative_dir() {
        let body = MINIMAL.replace(r#"dir = "alpha-loop""#, r#"dir = "subdir/nested""#);
        parse(&body).expect("legitimate nested relative dir");
    }

    #[test]
    fn rejects_absolute_tick_script() {
        let body = MINIMAL.replace(
            r#"tick_script = "skill/scripts/tick.py""#,
            r#"tick_script = "/usr/bin/whatever.py""#,
        );
        let err = parse(&body).unwrap_err().to_string();
        assert!(err.contains("must be a relative path"), "got: {err}");
    }

    #[test]
    fn rejects_tick_script_with_parent_traversal() {
        let body = MINIMAL.replace(
            r#"tick_script = "skill/scripts/tick.py""#,
            r#"tick_script = "../../etc/passwd""#,
        );
        let err = parse(&body).unwrap_err().to_string();
        assert!(err.contains("must be a relative path"), "got: {err}");
    }

    #[test]
    fn rejects_group_key_with_special_chars() {
        // Newline, double quote, and shell metacharacters must all be
        // rejected — they could escape AppleScript / shell quoting in
        // terminal backends.
        for bad in [
            "1\nevil",
            "\"; do shell script \"evil\"; \"",
            "1$(rm -rf /)",
            "1;ls",
            "1 2",
        ] {
            let body = MINIMAL.replace(r#"group_key = "2""#, &format!(r#"group_key = {:?}"#, bad));
            let err = parse(&body)
                .unwrap_err()
                .to_string();
            assert!(
                err.contains("group_key"),
                "expected group_key rejection for {bad:?}, got: {err}"
            );
        }
    }

    #[test]
    fn accepts_safe_group_keys_and_tab_titles() {
        for ok in ["workbench-1", "3", "central", "alpha.loop", "beta_loop", "x-1.2_3"] {
            let body = MINIMAL.replace(r#"group_key = "2""#, &format!(r#"group_key = "{ok}""#));
            parse(&body).unwrap_or_else(|e| panic!("group_key={ok:?} should pass: {e}"));
        }
    }

    #[test]
    fn rejects_tab_title_with_injection_payload() {
        let payload = r#""; do shell script "evil"; ""#;
        let body = MINIMAL.replace(
            r#"tick_script = "skill/scripts/tick.py""#,
            &format!(
                "tick_script = \"skill/scripts/tick.py\"\ntab_title = {:?}",
                payload
            ),
        );
        let err = parse(&body).unwrap_err().to_string();
        assert!(err.contains("tab_title"), "got: {err}");
    }

    #[test]
    fn round_trip_omits_none_tab_title_and_tick_script() {
        let body = r#"
schema_version = 1

[workbench]
claude_loops_dir = "~/x"
terminal = "konsole"
central_group = "1"

[[loops]]
name = "alpha"
dir = "alpha"
readme_anchor = "alpha"
group_key = "7"
schedule = "none"
dispatch = "resume_prompt"
"#;
        let cfg = parse(body).unwrap();
        let out = toml::to_string_pretty(&cfg).unwrap();
        assert!(!out.contains("tab_title"), "leaked tab_title:\n{out}");
        assert!(!out.contains("tick_script"), "leaked tick_script:\n{out}");
        assert!(!out.contains("tick_args"), "leaked tick_args:\n{out}");
    }
}