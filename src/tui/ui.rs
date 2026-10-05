//! Rendering. Pure functions of [`App`] state.

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{
    Block, BorderType, Cell, Clear, List, ListItem, Paragraph, Row, Sparkline, Table, Tabs, Wrap,
};

use super::app::{App, Focus, Popup, Tab};
use crate::protocol::{CoreState, LogEntry, LogSource};
use crate::util::{fmt_bytes, fmt_clock, fmt_duration, fmt_speed, now_unix};

const ACCENT: Color = Color::Cyan;
const DIM: Color = Color::DarkGray;
const UP: Color = Color::LightMagenta;
const DOWN: Color = Color::LightGreen;
const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

pub fn draw(frame: &mut Frame, app: &mut App) {
    let [header, tabs, body, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .areas(frame.area());

    draw_header(frame, header, app);
    draw_tabs(frame, tabs, app);
    match app.tab {
        Tab::Overview => draw_overview(frame, body, app),
        Tab::Proxies => draw_proxies(frame, body, app),
        Tab::Connections => draw_connections(frame, body, app),
        Tab::Logs => draw_logs(frame, body, app),
    }
    draw_footer(frame, footer, app);

    match &app.popup {
        Some(Popup::Help) => draw_help(frame),
        Some(Popup::Confirm { message, .. }) => draw_confirm(frame, message),
        Some(Popup::Message { title, body, error }) => draw_message(frame, title, body, *error),
        None => {}
    }
}

fn state_color(state: CoreState) -> Color {
    match state {
        CoreState::Running => Color::Green,
        CoreState::Starting | CoreState::Stopping | CoreState::Backoff => Color::Yellow,
        CoreState::Failed => Color::Red,
        CoreState::Stopped => Color::Gray,
    }
}

fn badge(state: CoreState) -> Span<'static> {
    Span::styled(
        format!(" {} ", state.label()),
        Style::new().fg(Color::Black).bg(state_color(state)).bold(),
    )
}

fn panel(title: &str) -> Block<'static> {
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(DIM))
        .title(Span::styled(
            format!(" {title} "),
            Style::new().fg(ACCENT).bold(),
        ))
}

fn draw_header(frame: &mut Frame, area: Rect, app: &App) {
    let mut left = vec![
        Span::styled(
            " singbox-board ",
            Style::new().fg(Color::Black).bg(ACCENT).bold(),
        ),
        Span::raw(" "),
    ];
    match (&app.status, &app.status_error) {
        (Some(status), _) => {
            left.push(badge(status.state));
            left.push(Span::raw(" sing-box "));
            left.push(Span::styled(
                status
                    .core_version
                    .clone()
                    .unwrap_or_else(|| "not installed".to_owned()),
                Style::new().bold(),
            ));
            if let Some(started) = status.started_at {
                left.push(Span::styled(
                    format!("  up {}", fmt_duration(now_unix().saturating_sub(started))),
                    Style::new().fg(DIM),
                ));
            }
            if let Some(mode) = app
                .configs
                .as_ref()
                .map(|c| c.mode.as_str())
                .filter(|m| !m.is_empty())
            {
                left.push(Span::styled("  mode ", Style::new().fg(DIM)));
                left.push(Span::styled(mode.to_owned(), Style::new().fg(ACCENT)));
            }
        }
        (None, Some(err)) => left.push(Span::styled(first_line(err), Style::new().fg(Color::Red))),
        (None, None) => left.push(Span::styled("connecting to daemon…", Style::new().fg(DIM))),
    }
    let left = Line::from(left);
    // Right-hand daemon info only when it fits next to the left part.
    let right = app.status.as_ref().map(|status| {
        Line::styled(
            format!(
                "daemon v{} · pid {} ",
                status.daemon_version, status.daemon_pid
            ),
            Style::new().fg(DIM),
        )
    });
    let right_width = right.as_ref().map_or(0, |r| r.width() as u16);
    if right_width > 0 && left.width() as u16 + right_width < area.width {
        let [left_area, right_area] =
            Layout::horizontal([Constraint::Min(0), Constraint::Length(right_width)]).areas(area);
        frame.render_widget(Paragraph::new(left), left_area);
        frame.render_widget(Paragraph::new(right.unwrap_or_default()), right_area);
    } else {
        frame.render_widget(Paragraph::new(left), area);
    }
}

fn draw_tabs(frame: &mut Frame, area: Rect, app: &App) {
    let titles = Tab::ALL
        .iter()
        .enumerate()
        .map(|(i, tab)| Line::from(format!("{} {}", i + 1, tab.title())));
    let tabs = Tabs::new(titles)
        .select(app.tab.index())
        .style(Style::new().fg(DIM))
        .highlight_style(
            Style::new()
                .fg(ACCENT)
                .bold()
                .add_modifier(Modifier::UNDERLINED),
        )
        .divider(Span::styled("│", Style::new().fg(DIM)));
    frame.render_widget(tabs, area);
}

// ----- overview -------------------------------------------------------------

fn kv(key: &str, value: impl Into<Span<'static>>) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{key:<11}"), Style::new().fg(DIM)),
        value.into(),
    ])
}

fn draw_overview(frame: &mut Frame, area: Rect, app: &App) {
    let [top, bottom] = Layout::vertical([Constraint::Length(13), Constraint::Min(3)]).areas(area);
    let [core_area, right] =
        Layout::horizontal([Constraint::Percentage(45), Constraint::Percentage(55)]).areas(top);
    let [traffic_area, clash_area] =
        Layout::vertical([Constraint::Length(8), Constraint::Min(0)]).areas(right);

    draw_core_panel(frame, core_area, app);
    draw_traffic_panel(frame, traffic_area, app);
    draw_clash_panel(frame, clash_area, app);

    let block = panel("Recent logs");
    let inner = block.inner(bottom);
    frame.render_widget(block, bottom);
    let height = inner.height as usize;
    let start = app.logs.len().saturating_sub(height);
    let lines: Vec<Line> = app.logs.iter().skip(start).map(log_line).collect();
    frame.render_widget(Paragraph::new(lines), inner);
}

fn draw_core_panel(frame: &mut Frame, area: Rect, app: &App) {
    let now = now_unix();
    let lines = match &app.status {
        None => vec![Line::from(Span::styled(
            app.status_error
                .clone()
                .unwrap_or_else(|| "waiting for daemon…".to_owned()),
            Style::new().fg(Color::Red),
        ))],
        Some(status) => {
            let mut lines = vec![
                kv("State", badge(status.state)),
                kv("PID", status.pid.map_or("-".to_owned(), |p| p.to_string())),
                kv(
                    "Uptime",
                    status
                        .started_at
                        .map_or("-".to_owned(), |s| fmt_duration(now.saturating_sub(s))),
                ),
                kv(
                    "Version",
                    status
                        .core_version
                        .clone()
                        .unwrap_or_else(|| "not installed (press u)".to_owned()),
                ),
                kv("Binary", status.binary.clone()),
                kv("Restarts", status.restarts.to_string()),
                kv(
                    "Last exit",
                    Span::styled(
                        status.last_exit.clone().unwrap_or_else(|| "-".to_owned()),
                        Style::new().fg(if status.state == CoreState::Failed {
                            Color::Red
                        } else {
                            Color::Reset
                        }),
                    ),
                ),
            ];
            if let Some(at) = status.next_restart_at {
                lines.push(kv(
                    "Restart in",
                    Span::styled(
                        format!("{}s", at.saturating_sub(now)),
                        Style::new().fg(Color::Yellow),
                    ),
                ));
            }
            if status.update_in_progress {
                lines.push(kv(
                    "Update",
                    Span::styled("in progress…", Style::new().fg(Color::Yellow)),
                ));
            }
            lines.push(kv(
                "Daemon",
                format!(
                    "v{} · up {}",
                    status.daemon_version,
                    fmt_duration(now.saturating_sub(status.daemon_started_at))
                ),
            ));
            lines.push(kv("Socket", app.socket()));
            lines
        }
    };
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: true })
            .block(panel("sing-box")),
        area,
    );
}

fn draw_traffic_panel(frame: &mut Frame, area: Rect, app: &App) {
    let block = panel("Traffic");
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let [up_label, up_spark, down_label, down_spark] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(2),
        Constraint::Length(1),
        Constraint::Length(2),
    ])
    .areas(inner);
    let t = &app.traffic;
    let width = inner.width as usize;
    let label = |arrow: &str, color: Color, speed: u64, total: u64| {
        let speed = format!("{arrow} {:>12}", fmt_speed(speed));
        let total = format!("   total {}", fmt_bytes(total));
        let mut spans = vec![Span::styled(speed.clone(), Style::new().fg(color).bold())];
        // Drop the total rather than clip it in narrow terminals.
        if speed.chars().count() + total.chars().count() <= width {
            spans.push(Span::styled(total, Style::new().fg(DIM)));
        }
        Line::from(spans)
    };
    frame.render_widget(
        Paragraph::new(label("↑", UP, t.up_speed, t.up_total)),
        up_label,
    );
    frame.render_widget(
        Paragraph::new(label("↓", DOWN, t.down_speed, t.down_total)),
        down_label,
    );
    let max = t
        .up_history
        .iter()
        .chain(t.down_history.iter())
        .copied()
        .max()
        .unwrap_or(0)
        .max(1);
    for (history, color, area) in [
        (&t.up_history, UP, up_spark),
        (&t.down_history, DOWN, down_spark),
    ] {
        let width = area.width as usize;
        let data: Vec<u64> = history
            .iter()
            .skip(history.len().saturating_sub(width))
            .copied()
            .collect();
        frame.render_widget(
            Sparkline::default()
                .data(&data)
                .max(max)
                .style(Style::new().fg(color)),
            area,
        );
    }
}

fn draw_clash_panel(frame: &mut Frame, area: Rect, app: &App) {
    let api = app.status.as_ref().and_then(|s| s.clash_api.as_ref());
    let mut lines = vec![kv(
        "API",
        match api {
            Some(api) => Span::raw(api.url.clone()),
            None => Span::styled(
                "not configured (experimental.clash_api)",
                Style::new().fg(Color::Yellow),
            ),
        },
    )];
    if let Some(err) = &app.clash_error {
        lines.push(kv(
            "Error",
            Span::styled(first_line(err), Style::new().fg(Color::Red)),
        ));
    } else if api.is_some() {
        let mode = app
            .configs
            .as_ref()
            .map_or("-".to_owned(), |c| c.mode.clone());
        lines.push(kv("Mode", Span::styled(mode, Style::new().fg(ACCENT))));
        lines.push(kv(
            "Conns",
            format!(
                "{}   memory {}",
                app.connections.len(),
                fmt_bytes(app.traffic.memory)
            ),
        ));
    }
    frame.render_widget(Paragraph::new(lines).block(panel("Clash API")), area);
}

// ----- proxies ---------------------------------------------------------------

fn delay_span(app: &App, name: &str) -> Span<'static> {
    if app.testing.contains(name) {
        return Span::styled("testing", Style::new().fg(DIM));
    }
    match app.delay_of(name) {
        None => Span::styled("-", Style::new().fg(DIM)),
        Some(Err(_)) => Span::styled("timeout", Style::new().fg(Color::Red)),
        Some(Ok(ms)) => {
            let color = match ms {
                0..300 => Color::Green,
                300..800 => Color::Yellow,
                _ => Color::Red,
            };
            Span::styled(format!("{ms} ms"), Style::new().fg(color))
        }
    }
}

fn draw_proxies(frame: &mut Frame, area: Rect, app: &mut App) {
    let [groups_area, members_area] =
        Layout::horizontal([Constraint::Percentage(35), Constraint::Percentage(65)]).areas(area);

    if let Some(err) = app.clash_error.as_ref().filter(|_| app.groups.is_empty()) {
        let text = Paragraph::new(first_line(err))
            .fg(Color::Red)
            .block(panel("Groups"));
        frame.render_widget(text, area);
        return;
    }

    let items: Vec<ListItem> = app
        .groups
        .iter()
        .map(|name| {
            let group = app.group(name);
            let kind = group.map(|g| g.kind.clone()).unwrap_or_default();
            let now = group.and_then(|g| g.now.clone()).unwrap_or_default();
            ListItem::new(vec![
                Line::from(vec![
                    Span::styled(name.clone(), Style::new().bold()),
                    Span::styled(format!("  {kind}"), Style::new().fg(DIM)),
                ]),
                Line::from(Span::styled(format!("  → {now}"), Style::new().fg(ACCENT))),
            ])
        })
        .collect();
    let groups_focused = app.focus == Focus::Groups;
    let list = List::new(items)
        .block(
            panel(&format!("Groups ({})", app.groups.len()))
                .border_style(focus_border(groups_focused)),
        )
        .highlight_style(highlight(groups_focused))
        .highlight_symbol("▌");
    frame.render_stateful_widget(list, groups_area, &mut app.group_state);

    let group_name = app.selected_group().unwrap_or_default().to_owned();
    let group = app.group(&group_name);
    let now = group.and_then(|g| g.now.clone()).unwrap_or_default();
    let selectable = group.is_some_and(|g| g.is_selectable());
    let rows: Vec<Row> = app
        .members()
        .into_iter()
        .map(|member| {
            let active = member == now;
            let kind = app
                .group(&member)
                .map(|p| p.kind.clone())
                .unwrap_or_default();
            Row::new(vec![
                Cell::from(if active {
                    Span::styled("●", Style::new().fg(Color::Green))
                } else {
                    Span::raw(" ")
                }),
                Cell::from(Span::styled(
                    member.clone(),
                    if active {
                        Style::new().fg(Color::Green).bold()
                    } else {
                        Style::new()
                    },
                )),
                Cell::from(Span::styled(kind, Style::new().fg(DIM))),
                Cell::from(delay_span(app, &member)),
            ])
        })
        .collect();
    let title = format!(
        "{group_name} {}",
        if selectable {
            "· Enter select · t test"
        } else {
            "· t test"
        }
    );
    let members_focused = app.focus == Focus::Members;
    let table = Table::new(
        rows,
        [
            Constraint::Length(1),
            Constraint::Fill(1),
            Constraint::Length(14),
            Constraint::Length(8),
        ],
    )
    .header(Row::new(["", "Name", "Type", "Delay"]).style(Style::new().fg(DIM)))
    .block(panel(&title).border_style(focus_border(members_focused)))
    .row_highlight_style(highlight(members_focused))
    .highlight_symbol("▌");
    frame.render_stateful_widget(table, members_area, &mut app.member_state);
}

fn focus_border(focused: bool) -> Style {
    Style::new().fg(if focused { ACCENT } else { DIM })
}

fn highlight(focused: bool) -> Style {
    if focused {
        Style::new().bg(Color::Rgb(40, 60, 80)).bold()
    } else {
        Style::new().bg(Color::Rgb(40, 40, 40))
    }
}

// ----- connections -----------------------------------------------------------

fn age(start: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(start)
        .map(|t| {
            let secs = chrono::Utc::now()
                .signed_duration_since(t)
                .num_seconds()
                .max(0);
            fmt_duration(secs as u64)
        })
        .unwrap_or_default()
}

fn draw_connections(frame: &mut Frame, area: Rect, app: &mut App) {
    let title = format!(
        "Connections ({}) · ↑ {} ↓ {} · d close · D close all",
        app.connections.len(),
        fmt_speed(app.traffic.up_speed),
        fmt_speed(app.traffic.down_speed)
    );
    if let Some(err) = &app.clash_error {
        frame.render_widget(
            Paragraph::new(first_line(err))
                .fg(Color::Red)
                .block(panel(&title)),
            area,
        );
        return;
    }
    let rows: Vec<Row> = app
        .connections
        .iter()
        .map(|c| {
            Row::new(vec![
                Cell::from(c.target()),
                Cell::from(Span::styled(
                    c.metadata.network.clone(),
                    Style::new().fg(DIM),
                )),
                // sing-box lists the final node first; show group → node instead.
                Cell::from(
                    c.chains
                        .iter()
                        .rev()
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(" → "),
                ),
                Cell::from(Span::styled(c.rule.clone(), Style::new().fg(DIM))),
                Cell::from(Span::styled(fmt_bytes(c.upload), Style::new().fg(UP))),
                Cell::from(Span::styled(fmt_bytes(c.download), Style::new().fg(DOWN))),
                Cell::from(age(&c.start)),
            ])
        })
        .collect();
    let table = Table::new(
        rows,
        [
            Constraint::Fill(3),
            Constraint::Length(4),
            Constraint::Fill(2),
            Constraint::Fill(2),
            Constraint::Length(10),
            Constraint::Length(10),
            Constraint::Length(9),
        ],
    )
    .header(
        Row::new(["Destination", "Net", "Chain", "Rule", "Up", "Down", "Age"])
            .style(Style::new().fg(DIM)),
    )
    .block(panel(&title))
    .row_highlight_style(highlight(true))
    .highlight_symbol("▌");
    frame.render_stateful_widget(table, area, &mut app.conn_state);
}

// ----- logs ------------------------------------------------------------------

const LEVELS: [(&str, Color); 7] = [
    ("PANIC", Color::Red),
    ("FATAL", Color::Red),
    ("ERROR", Color::Red),
    ("WARN", Color::Yellow),
    ("INFO", Color::Green),
    ("DEBUG", Color::Blue),
    ("TRACE", Color::DarkGray),
];

/// Locates the level token of a sing-box log line. sing-box writes
/// `INFO[0000] message` when stdout is not a terminal and
/// `+0800 2026-01-02 15:04:05 INFO message` with `log.timestamp` enabled.
fn find_level(line: &str) -> Option<(usize, usize, Color)> {
    for (level, color) in LEVELS {
        if let Some(rest) = line.strip_prefix(level)
            && rest.starts_with('[')
        {
            // Include the `[0000]` uptime counter in the highlighted token.
            let end = rest.find(']').map_or(level.len(), |i| level.len() + i + 1);
            return Some((0, end, color));
        }
    }
    LEVELS.iter().find_map(|(level, color)| {
        let index = line.find(&format!(" {level} "))? + 1;
        Some((index, index + level.len(), *color))
    })
}

fn log_line(entry: &LogEntry) -> Line<'static> {
    let line = &entry.line;
    match entry.source {
        LogSource::Daemon => Line::from(vec![
            Span::styled(fmt_clock(entry.ts), Style::new().fg(DIM)),
            Span::styled(" daemon ", Style::new().fg(ACCENT).bold()),
            Span::raw(line.clone()),
        ]),
        LogSource::Core => match find_level(line) {
            Some((start, end, color)) => Line::from(vec![
                Span::styled(line[..start].to_owned(), Style::new().fg(DIM)),
                Span::styled(line[start..end].to_owned(), Style::new().fg(color).bold()),
                Span::raw(line[end..].to_owned()),
            ]),
            None => Line::raw(line.clone()),
        },
    }
}

fn draw_logs(frame: &mut Frame, area: Rect, app: &App) {
    let follow = if app.log_scroll == 0 {
        "following".to_owned()
    } else {
        format!("scrolled ↑{} · End to follow", app.log_scroll)
    };
    let connection = if app.logs_connected {
        ""
    } else {
        " · disconnected"
    };
    let block = panel(&format!("Logs ({}) · {follow}{connection}", app.logs.len()));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let height = inner.height as usize;
    let end = app.logs.len().saturating_sub(app.log_scroll);
    let start = end.saturating_sub(height);
    let lines: Vec<Line> = app.logs.range(start..end).map(log_line).collect();
    frame.render_widget(Paragraph::new(lines), inner);
}

// ----- footer & popups -------------------------------------------------------

fn draw_footer(frame: &mut Frame, area: Rect, app: &App) {
    let keys: &[(&str, &str)] = match app.tab {
        Tab::Proxies => &[
            ("←→", "focus"),
            ("⏎", "select"),
            ("t", "test"),
            ("T", "test one"),
        ],
        Tab::Connections => &[("↑↓", "move"), ("d", "close"), ("D", "close all")],
        Tab::Logs => &[("↑↓", "scroll"), ("PgUp/PgDn", "page"), ("End", "follow")],
        Tab::Overview => &[],
    };
    let global: [(&str, &str); 8] = [
        ("s", "start"),
        ("x", "stop"),
        ("r", "restart"),
        ("R", "reload"),
        ("c", "check"),
        ("u", "update"),
        ("m", "mode"),
        ("?", "help"),
    ];
    let mut spans = Vec::new();
    for (key, label) in keys.iter().chain(global.iter()) {
        spans.push(Span::styled(
            format!(" {key}"),
            Style::new().fg(ACCENT).bold(),
        ));
        spans.push(Span::styled(format!(" {label}"), Style::new().fg(DIM)));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);

    let right = if let Some((_, label)) = app.busy.last() {
        Some(Span::styled(
            format!("{} {label}… ", SPINNER[app.frame % SPINNER.len()]),
            Style::new().fg(Color::Yellow),
        ))
    } else {
        app.toast.as_ref().map(|toast| {
            Span::styled(
                format!(" {} ", toast.text),
                if toast.error {
                    Style::new().fg(Color::White).bg(Color::Red)
                } else {
                    Style::new().fg(Color::Black).bg(Color::Green)
                },
            )
        })
    };
    if let Some(span) = right {
        let width = (span.width() as u16).min(area.width);
        let rect = Rect {
            x: area.right() - width,
            width,
            ..area
        };
        frame.render_widget(Clear, rect);
        frame.render_widget(Paragraph::new(span), rect);
    }
}

fn popup_area(frame: &Frame, width: u16, height: u16) -> Rect {
    let area = frame.area();
    area.centered(
        Constraint::Length(width.min(area.width)),
        Constraint::Length(height.min(area.height)),
    )
}

fn draw_help(frame: &mut Frame) {
    let rows = [
        ("Global", ""),
        ("1-4 / Tab", "switch tab"),
        ("s / x / r", "start / stop / restart sing-box"),
        ("R", "check config and hot-reload (SIGHUP)"),
        ("c", "run sing-box check"),
        ("u", "check MiChongs/sing-box releases and update"),
        ("m", "cycle Clash mode"),
        ("q / Ctrl-C", "quit"),
        ("Proxies", ""),
        ("↑↓ / j k", "move"),
        ("←→ / h l", "groups ↔ nodes"),
        ("Enter", "select node (Selector / URLTest / Smart)"),
        ("t / T", "delay test group / node"),
        ("Connections", ""),
        ("d / D", "close selected / all"),
        ("Logs", ""),
        ("↑↓ PgUp PgDn", "scroll, End follows"),
    ];
    let lines: Vec<Line> = rows
        .iter()
        .map(|(key, desc)| {
            if desc.is_empty() {
                Line::from(Span::styled(*key, Style::new().fg(ACCENT).bold()))
            } else {
                Line::from(vec![
                    Span::styled(format!("  {key:<14}"), Style::new().bold()),
                    Span::raw(*desc),
                ])
            }
        })
        .collect();
    let area = popup_area(frame, 64, lines.len() as u16 + 2);
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(lines).block(panel("Keys · any key to close")),
        area,
    );
}

fn draw_confirm(frame: &mut Frame, message: &str) {
    let mut text = Text::from(message.to_owned());
    text.push_line(Line::raw(""));
    text.push_line(Line::from(vec![
        Span::styled("y", Style::new().fg(Color::Green).bold()),
        Span::raw(" confirm   "),
        Span::styled("n", Style::new().fg(Color::Red).bold()),
        Span::raw(" cancel"),
    ]));
    let height = text.height() as u16 + 2;
    let area = popup_area(frame, 56, height);
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(text)
            .alignment(Alignment::Center)
            .wrap(Wrap { trim: false })
            .block(panel("Confirm").border_style(Style::new().fg(Color::Yellow))),
        area,
    );
}

fn draw_message(frame: &mut Frame, title: &str, body: &str, error: bool) {
    let area = frame.area();
    let width = (area.width * 4 / 5).max(40);
    let height = (body.lines().count() as u16 + 4)
        .min(area.height * 4 / 5)
        .max(5);
    let rect = popup_area(frame, width, height);
    frame.render_widget(Clear, rect);
    let color = if error { Color::Red } else { Color::Green };
    frame.render_widget(
        Paragraph::new(body.to_owned())
            .wrap(Wrap { trim: false })
            .block(panel(&format!("{title} · Esc to close")).border_style(Style::new().fg(color))),
        rect,
    );
}

fn first_line(text: &str) -> String {
    text.lines().next().unwrap_or_default().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn level_detection() {
        let (start, end, color) = find_level("WARN[0012] dns: lookup failed").unwrap();
        assert_eq!((start, end, color), (0, 10, Color::Yellow));
        let line = "+0800 2026-10-05 12:00:00 ERROR router: oops";
        let (start, end, color) = find_level(line).unwrap();
        assert_eq!((&line[start..end], color), ("ERROR", Color::Red));
        assert!(find_level("plain text").is_none());
    }

    #[test]
    fn level_token_must_be_exact() {
        assert!(find_level("INFORMATION about something").is_none());
        assert_eq!(
            find_level("INFO[0000] started").unwrap(),
            (0, 10, Color::Green)
        );
    }
}
