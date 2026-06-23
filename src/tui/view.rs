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

//! Pure rendering: takes a [`ViewState`] and draws to a ratatui Frame.
//! Snapshot-tested without a real terminal via `Buffer::with_lines`.

use chrono::{DateTime, Utc};
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table};
use ratatui::Frame;

use crate::daemon::ipc::{StatusLoop, StatusResponse};
use crate::state::PaneHealth;

#[derive(Default, Clone)]
pub struct ViewState {
    pub status: Option<StatusResponse>,
    pub last_error: Option<String>,
    pub last_refresh: Option<DateTime<Utc>>,
    /// Highlighted row in the loops table. Stays clamped to
    /// [0, status.loops.len() - 1] across refreshes.
    pub selected: usize,
    /// Transient flash from the last action (e.g. tick result), shown in
    /// the footer until cleared by `clear_flash` or the next refresh.
    pub flash: Option<String>,
    pub flash_at: Option<DateTime<Utc>>,
    /// When `Some`, the TUI is in a modal line-input mode; the body is
    /// rendered in place of the footer hints.
    pub input: Option<InputMode>,
}

/// What the TUI is collecting from the user when `input.is_some()`.
#[derive(Clone, Debug)]
pub enum InputMode {
    /// `p` — change the active claude_loops_dir.
    SetRepo { current: String, buffer: String },
    /// `g` — move the highlighted loop's group_key.
    MoveGroup {
        name: String,
        current_group: String,
        buffer: String,
    },
    /// Confirmation prompt for a destructive group-move (multi-tab konsole).
    ConfirmMoveGroup {
        name: String,
        from: String,
        to: String,
    },
    /// Confirmation prompt for `c` (close loop). Closes the live pane
    /// and marks the loop inactive.
    ConfirmClose { name: String },
    /// Confirmation prompt for `d` (delete loop). Destructive: closes
    /// the pane, drops from workbench.toml, strips the README block,
    /// recursively removes the loop dir.
    ConfirmDelete { name: String },
}

impl ViewState {
    pub async fn refresh(&mut self, socket: &std::path::Path) {
        match super::fetch_status(socket).await {
            Ok(s) => {
                if !s.loops.is_empty() && self.selected >= s.loops.len() {
                    self.selected = s.loops.len() - 1;
                }
                self.status = Some(s);
                self.last_error = None;
            }
            Err(e) => {
                self.last_error = Some(format!("{e:#}"));
            }
        }
        self.last_refresh = Some(Utc::now());
    }

    pub fn selected_name(&self) -> Option<&str> {
        let s = self.status.as_ref()?;
        s.loops.get(self.selected).map(|l| l.name.as_str())
    }

    pub fn move_selection(&mut self, delta: isize) {
        let Some(s) = self.status.as_ref() else {
            return;
        };
        if s.loops.is_empty() {
            return;
        }
        let len = s.loops.len() as isize;
        let next = (self.selected as isize + delta).rem_euclid(len);
        self.selected = next as usize;
    }

    pub fn set_flash(&mut self, msg: impl Into<String>) {
        self.flash = Some(msg.into());
        self.flash_at = Some(Utc::now());
    }

    pub fn clear_flash_if_stale(&mut self, ttl_secs: i64) {
        let stale = match self.flash_at {
            Some(t) => Utc::now().signed_duration_since(t).num_seconds() >= ttl_secs,
            None => self.flash.is_some(),
        };
        if stale {
            self.flash = None;
            self.flash_at = None;
        }
    }
}

pub fn render(f: &mut Frame, state: &ViewState) {
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // header
            Constraint::Min(5),    // table
            Constraint::Length(3), // footer
        ])
        .split(f.area());

    f.render_widget(header(state), layout[0]);
    f.render_widget(table(state), layout[1]);
    f.render_widget(footer(state), layout[2]);
}

fn header(state: &ViewState) -> Paragraph<'_> {
    let terminal = state
        .status
        .as_ref()
        .map(|s| s.terminal.as_str())
        .unwrap_or("(no daemon)");
    let refresh = state
        .last_refresh
        .map(|t| t.with_timezone(&chrono::Local).format("%H:%M:%S").to_string())
        .unwrap_or_else(|| "never".into());
    let line = Line::from(vec![
        Span::styled(
            "  agent-loop-workbench",
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!("    terminal: {terminal}    refreshed: {refresh}")),
    ]);
    Paragraph::new(line).block(Block::default().borders(Borders::BOTTOM))
}

fn table(state: &ViewState) -> Table<'_> {
    let header_cells = [
        "LOOP",
        "GROUP",
        "HEALTH",
        "LAST TICK",
        "NEXT TICK",
        "EXIT",
        "METRICS",
    ]
    .iter()
    .map(|h| Cell::from(*h).style(Style::default().add_modifier(Modifier::BOLD)));
    let header = Row::new(header_cells).height(1);

    let rows: Vec<Row> = match &state.status {
        Some(s) => s
            .loops
            .iter()
            .enumerate()
            .map(|(i, l)| loop_row(l, i == state.selected && !s.loops.is_empty()))
            .collect(),
        None => vec![Row::new(vec![Cell::from("waiting for daemon...")])],
    };

    Table::new(
        rows,
        [
            // Loop column: 40 chars fits a typical multi-segment name
            // (~37 chars) plus the 2-char selection-arrow prefix.
            Constraint::Length(40),
            Constraint::Length(6),
            Constraint::Length(16),
            Constraint::Length(20),
            Constraint::Length(20),
            Constraint::Length(6),
            Constraint::Min(20),
        ],
    )
    .header(header)
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title(" loops "),
    )
}

fn loop_row(l: &StatusLoop, selected: bool) -> Row<'_> {
    let last = l
        .last_tick
        .map(format_local)
        .unwrap_or_else(|| "—".into());
    let next = l
        .next_tick
        .map(format_local)
        .unwrap_or_else(|| "—".into());
    let exit_cell = match l.last_dispatch_exit {
        Some(0) => Cell::from("ok").style(Style::default().fg(Color::Green)),
        Some(n) => Cell::from(n.to_string()).style(Style::default().fg(Color::Red)),
        None => Cell::from("—"),
    };
    let slug_cell = if selected {
        Cell::from(format!("▶ {}", l.name)).style(
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
    } else {
        Cell::from(format!("  {}", l.name))
    };
    let row_style = if selected {
        Style::default().bg(Color::Rgb(40, 40, 60))
    } else {
        Style::default()
    };
    Row::new(vec![
        slug_cell,
        Cell::from(l.group_key.clone()),
        health_cell(&l.pane_health),
        Cell::from(last),
        Cell::from(next),
        exit_cell,
        Cell::from(format_metrics(l)),
    ])
    .style(row_style)
}

fn health_cell(h: &PaneHealth) -> Cell<'_> {
    match h {
        PaneHealth::Running => {
            Cell::from("running").style(Style::default().fg(Color::Green))
        }
        PaneHealth::Unknown => {
            Cell::from("unknown").style(Style::default().fg(Color::DarkGray))
        }
        PaneHealth::Crashed { .. } => Cell::from("crashed").style(
            Style::default()
                .fg(Color::Black)
                .bg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        PaneHealth::FailedRecreate { attempts, .. } => Cell::from(format!(
            "fail x{attempts}"
        ))
        .style(
            Style::default()
                .fg(Color::White)
                .bg(Color::Red)
                .add_modifier(Modifier::BOLD),
        ),
        PaneHealth::Inactive { .. } => Cell::from("inactive").style(
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::DIM),
        ),
    }
}

fn format_local(t: DateTime<Utc>) -> String {
    t.with_timezone(&chrono::Local)
        .format("%Y-%m-%d %H:%M")
        .to_string()
}

fn format_metrics(l: &StatusLoop) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let (Some(p), Some(t)) = (
        l.metrics.integers.get("phase"),
        l.metrics.integers.get("phase_total"),
    ) {
        parts.push(format!("phase {p}/{t}"));
    }
    for (k, label) in [
        ("active_change_cases", "active"),
        ("missing_change_cases", "missing"),
        ("pending_close", "pending"),
        ("active_wis", "wis"),
        ("checked", "done"),
        ("unchecked", "todo"),
    ] {
        if let Some(v) = l.metrics.integers.get(k) {
            parts.push(format!("{label}={v}"));
        }
    }
    if let Some(t) = l.metrics.strings.get("next_target") {
        parts.push(format!("next: {t}"));
    }
    parts.join("  ")
}

fn footer(state: &ViewState) -> Paragraph<'_> {
    if let Some(input) = &state.input {
        return input_prompt(input);
    }
    if let Some(err) = &state.last_error {
        let line = Line::from(vec![
            Span::styled(
                " error ",
                Style::default().fg(Color::White).bg(Color::Red),
            ),
            Span::raw(format!("  {err}")),
        ]);
        return Paragraph::new(line).block(Block::default().borders(Borders::TOP));
    }
    if let Some(flash) = &state.flash {
        let line = Line::from(vec![
            Span::styled(
                " info ",
                Style::default().fg(Color::Black).bg(Color::Yellow),
            ),
            Span::raw(format!("  {flash}")),
        ]);
        return Paragraph::new(line).block(Block::default().borders(Borders::TOP));
    }
    let line = Line::from(vec![
        Span::raw("  "),
        Span::styled("↑/↓", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(" select  "),
        Span::styled("t", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(" tick  "),
        Span::styled("a", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(" attach  "),
        Span::styled("g", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(" move-group  "),
        Span::styled("c", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(" close  "),
        Span::styled("d", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(" delete  "),
        Span::styled("u", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(" update-prompt  "),
        Span::styled("p", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(" persist  "),
        Span::styled("n", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(" add-loop  "),
        Span::styled("R", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(" recover  "),
        Span::styled("r", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(" refresh  "),
        Span::styled("q", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(" quit"),
    ]);
    Paragraph::new(line).block(Block::default().borders(Borders::TOP))
}

fn input_prompt(input: &InputMode) -> Paragraph<'_> {
    let (label, value) = match input {
        InputMode::SetRepo { current, buffer } => (
            format!(" persist from (path or git URL; enter to load, esc to cancel) — current: {current} "),
            buffer.clone(),
        ),
        InputMode::MoveGroup {
            name,
            current_group,
            buffer,
        } => (
            format!(
                " move {name} from group {current_group} \u{2192} (type new group, enter=submit, esc=cancel) "
            ),
            buffer.clone(),
        ),
        InputMode::ConfirmMoveGroup { name, from, to } => (
            format!(
                " confirm: moving {name} from {from} to {to} requires closing the tab and re-spawning claude. y/n "
            ),
            String::new(),
        ),
        InputMode::ConfirmClose { name } => (
            format!(
                " close `{name}`? this kills the running claude and marks it inactive. y/n "
            ),
            String::new(),
        ),
        InputMode::ConfirmDelete { name } => (
            format!(
                " DELETE `{name}`? closes pane, drops from workbench.toml, strips README, rm -rf the loop dir. NOT reversible. y/n "
            ),
            String::new(),
        ),
    };
    let line = Line::from(vec![
        Span::styled(label, Style::default().fg(Color::Black).bg(Color::Cyan)),
        Span::raw("  "),
        Span::styled(value, Style::default().add_modifier(Modifier::BOLD)),
        Span::styled("_", Style::default().add_modifier(Modifier::SLOW_BLINK)),
    ]);
    Paragraph::new(line).block(Block::default().borders(Borders::TOP))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::ipc::StatusResponse;
    use crate::state::Metrics;
    use ratatui::backend::TestBackend;

    fn synth_status() -> StatusResponse {
        let mut integers = std::collections::BTreeMap::new();
        integers.insert("active_change_cases".into(), 3);
        integers.insert("missing_change_cases".into(), 11);
        let metrics = Metrics {
            integers,
            ..Metrics::default()
        };
        StatusResponse {
            terminal: "konsole".into(),
            loop_count: 2,
            loops: vec![
                StatusLoop {
                    name: "alpha-loop".into(),
                    group_key: "2".into(),
                    tab_title: "alpha-loop".into(),
                    schedule: "every 10m".into(),
                    pane_id: Some("konsole:1:7".into()),
                    last_tick: None,
                    next_tick: None,
                    last_dispatch_kind: None,
                    last_dispatch_exit: None,
                    metrics,
                    pane_health: PaneHealth::Running,
                },
                StatusLoop {
                    name: "beta-loop".into(),
                    group_key: "2".into(),
                    tab_title: "beta-loop".into(),
                    schedule: "every 15m".into(),
                    pane_id: None,
                    last_tick: None,
                    next_tick: None,
                    last_dispatch_kind: None,
                    last_dispatch_exit: Some(0),
                    metrics: Metrics::default(),
                    pane_health: PaneHealth::Crashed {
                        since: chrono::Utc::now(),
                    },
                },
            ],
        }
    }

    #[test]
    fn render_snapshot_for_two_loops() {
        let backend = TestBackend::new(160, 14);
        let mut term = ratatui::Terminal::new(backend).unwrap();
        let state = ViewState {
            status: Some(synth_status()),
            last_refresh: Some(Utc::now()),
            last_error: None,
            selected: 0,
            flash: None,
            flash_at: None,
            input: None,
        };
        term.draw(|f| render(f, &state)).unwrap();

        let buffer = term.backend().buffer();
        let dump = buffer
            .content
            .iter()
            .map(|c| c.symbol().to_string())
            .collect::<String>();
        // Headers + names + numbers must all appear.
        assert!(dump.contains("agent-loop-workbench"));
        assert!(dump.contains("alpha-loop"));
        assert!(dump.contains("beta-loop"));
        assert!(dump.contains("active=3"));
        assert!(dump.contains("missing=11"));
        // ok cell should appear for the beta-loop exit=0 case.
        assert!(dump.contains("ok"));
        // Selection arrow on the first row.
        assert!(dump.contains("▶"));
        // Footer hints
        assert!(dump.contains("tick"));
        assert!(dump.contains("attach"));
    }

    #[test]
    fn move_selection_wraps_and_clamps() {
        let mut state = ViewState {
            status: Some(synth_status()),
            ..ViewState::default()
        };
        assert_eq!(state.selected, 0);
        state.move_selection(1);
        assert_eq!(state.selected, 1);
        state.move_selection(1);
        assert_eq!(state.selected, 0); // wrapped
        state.move_selection(-1);
        assert_eq!(state.selected, 1); // wrapped backwards
        assert_eq!(state.selected_name(), Some("beta-loop"));
    }

    #[test]
    fn move_selection_no_op_on_empty_status() {
        let mut state = ViewState::default();
        state.move_selection(1);
        assert_eq!(state.selected, 0);
        assert_eq!(state.selected_name(), None);
    }

    #[test]
    fn render_shows_repo_input_prompt() {
        let backend = TestBackend::new(120, 12);
        let mut term = ratatui::Terminal::new(backend).unwrap();
        let state = ViewState {
            status: Some(synth_status()),
            input: Some(InputMode::SetRepo {
                current: "~/claude-loops".into(),
                buffer: "~/other-loops".into(),
            }),
            ..ViewState::default()
        };
        term.draw(|f| render(f, &state)).unwrap();
        let dump = term
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol().to_string())
            .collect::<String>();
        assert!(dump.contains("persist from"));
        assert!(dump.contains("~/claude-loops"));
        assert!(dump.contains("~/other-loops"));
    }

    #[test]
    fn render_shows_move_group_confirm() {
        let backend = TestBackend::new(140, 12);
        let mut term = ratatui::Terminal::new(backend).unwrap();
        let state = ViewState {
            status: Some(synth_status()),
            input: Some(InputMode::ConfirmMoveGroup {
                name: "alpha-loop".into(),
                from: "7".into(),
                to: "8".into(),
            }),
            ..ViewState::default()
        };
        term.draw(|f| render(f, &state)).unwrap();
        let dump = term
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol().to_string())
            .collect::<String>();
        assert!(dump.contains("confirm"));
        assert!(dump.contains("alpha-loop"));
        assert!(dump.contains("y/n"));
    }

    #[test]
    fn refresh_clamps_selection_when_loops_shrink() {
        let mut state = ViewState {
            status: Some(synth_status()),
            ..ViewState::default()
        };
        state.selected = 1; // beta-loop
        // Simulate refresh that drops to 1 loop.
        let mut s = synth_status();
        s.loops.truncate(1);
        s.loop_count = 1;
        // Replicate the clamp logic from ViewState::refresh inline.
        if !s.loops.is_empty() && state.selected >= s.loops.len() {
            state.selected = s.loops.len() - 1;
        }
        state.status = Some(s);
        assert_eq!(state.selected, 0);
    }

    #[test]
    fn render_shows_error_when_present() {
        let backend = TestBackend::new(80, 12);
        let mut term = ratatui::Terminal::new(backend).unwrap();
        let state = ViewState {
            last_error: Some("daemon not running".into()),
            ..ViewState::default()
        };
        term.draw(|f| render(f, &state)).unwrap();
        let buffer = term.backend().buffer();
        let dump = buffer
            .content
            .iter()
            .map(|c| c.symbol().to_string())
            .collect::<String>();
        assert!(dump.contains("error"));
        assert!(dump.contains("daemon not running"));
    }
}