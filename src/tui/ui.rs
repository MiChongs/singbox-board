//! Rendering. Pure functions of [`App`] state.

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Cell, Clear, List, ListItem, Paragraph, Row, Sparkline, Table, Wrap};

use super::app::{App, Focus, Popup, StoreFocus, Tab};
use super::core::{self as core_view, InputPurpose};
use super::theme::{
    ACCENT, BLUE, CRUST, DIM, DOWN, GREEN, MARK, RED, SKY, SPINNER, SUBTEXT, SURFACE, TEXT, UP,
    YELLOW, chip, delay_color, dim, field, header_row, key, label, panel, pill, selected,
    state_color, state_pill,
};
use crate::i18n::fl;
use crate::protocol::{
    Component, ComponentAction, ComponentStatus, CoreState, LogEntry, LogSource, variant_label,
    version_label,
};
use crate::util::{fmt_bytes, fmt_clock, fmt_duration, fmt_speed, join_list, now_unix, text_width};

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
        Tab::SubStore => draw_sub_store(frame, body, app),
        Tab::Core => core_view::draw(frame, body, app),
    }
    draw_footer(frame, footer, app);

    match &app.popup {
        Some(Popup::Help) => draw_help(frame),
        Some(Popup::Confirm { message, .. }) => draw_confirm(frame, message),
        Some(Popup::Message {
            title,
            body,
            error,
            copy,
        }) => draw_message(frame, title, body, *error, copy.is_some()),
        Some(Popup::Setup { sub_store }) => draw_setup(frame, *sub_store),
        Some(Popup::Menu {
            component,
            actions,
            selected,
        }) => draw_menu(frame, *component, actions, *selected),
        Some(Popup::Input {
            title,
            hint,
            value,
            purpose,
        }) => draw_input(frame, title, hint, value, *purpose),
        None => {}
    }
}

// ----- chrome ------------------------------------------------------------------

fn draw_header(frame: &mut Frame, area: Rect, app: &App) {
    let mut left = vec![
        Span::styled(
            " ◆ singbox-board ",
            Style::new().fg(CRUST).bg(ACCENT).bold(),
        ),
        Span::raw(" "),
    ];
    match (&app.status, &app.status_error) {
        (Some(status), _) => {
            left.push(state_pill(status.state));
            left.push(dim("  sing-box "));
            left.push(Span::styled(
                status
                    .core_version
                    .clone()
                    .unwrap_or_else(|| fl!("not-installed")),
                Style::new().fg(TEXT).bold(),
            ));
            if let Some(core) = &status.active_core {
                left.push(dim("  "));
                left.push(Span::styled(core.source_label(), Style::new().fg(ACCENT)));
                left.push(dim("  "));
                left.push(Span::styled(
                    variant_label(&core.variant),
                    Style::new().fg(BLUE),
                ));
            }
            if let Some(started) = status.started_at {
                left.push(dim("  "));
                left.push(dim(fl!(
                    "tui-uptime",
                    uptime = fmt_duration(now_unix().saturating_sub(started))
                )));
            }
            if let Some(mode) = app
                .configs
                .as_ref()
                .map(|c| c.mode.as_str())
                .filter(|m| !m.is_empty())
            {
                left.push(Span::raw("  "));
                left.push(chip(mode.to_lowercase(), SKY));
            }
        }
        (None, Some(err)) => left.push(Span::styled(first_line(err), Style::new().fg(RED))),
        (None, None) => left.push(dim(fl!("tui-connecting"))),
    }
    let left = Line::from(left);
    // Right-hand daemon info only when it fits next to the left part.
    let right = app.status.as_ref().map(|status| {
        Line::from(dim(format!(
            "{} ",
            fl!(
                "tui-header-daemon",
                version = status.daemon_version.clone(),
                pid = status.daemon_pid.to_string()
            )
        )))
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
    let mut spans = vec![Span::raw(" ")];
    for (i, tab) in Tab::ALL.iter().enumerate() {
        if *tab == app.tab {
            spans.push(Span::styled(
                format!(" {} {} ", i + 1, tab.title()),
                Style::new().fg(CRUST).bg(ACCENT).bold(),
            ));
        } else {
            spans.push(Span::styled(format!(" {}", i + 1), Style::new().fg(DIM)));
            spans.push(Span::styled(
                format!(" {} ", tab.title()),
                Style::new().fg(SUBTEXT),
            ));
        }
        spans.push(Span::raw(" "));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_footer(frame: &mut Frame, area: Rect, app: &App) {
    let keys: Vec<(&str, String)> = match app.tab {
        Tab::Proxies => vec![
            ("←→", fl!("key-focus")),
            ("⏎", fl!("key-select")),
            ("t", fl!("key-test")),
            ("T", fl!("key-test-one")),
        ],
        Tab::Connections => vec![
            ("↑↓", fl!("key-move")),
            ("d", fl!("key-close")),
            ("D", fl!("key-close-all")),
        ],
        Tab::Logs => vec![
            ("↑↓", fl!("key-scroll")),
            ("PgUp/Dn", fl!("key-page")),
            ("End", fl!("key-follow")),
        ],
        Tab::SubStore => vec![
            ("←→", fl!("key-focus")),
            ("⏎", fl!("key-actions")),
            ("y", fl!("key-copy-url")),
            ("w", fl!("key-web-ui")),
        ],
        Tab::Core => core_view::hints(app),
        Tab::Overview => Vec::new(),
    };
    let global = [
        ("s", fl!("key-start")),
        ("x", fl!("key-stop")),
        ("r", fl!("key-restart")),
        ("R", fl!("key-reload")),
        ("u", fl!("key-update")),
        ("m", fl!("key-mode")),
        ("?", fl!("key-help")),
    ];
    let mut spans = vec![Span::raw(" ")];
    for (i, (k, label)) in keys.iter().chain(global.iter()).enumerate() {
        if i == keys.len() && !keys.is_empty() {
            spans.push(Span::styled("│ ", Style::new().fg(SURFACE)));
        }
        spans.push(key(k));
        spans.push(dim(format!(" {label}  ")));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);

    let right = if let Some((_, label)) = app.busy.last() {
        Some(Span::styled(
            format!(" {} {label} ", SPINNER[app.frame % SPINNER.len()]),
            Style::new().fg(CRUST).bg(YELLOW).bold(),
        ))
    } else {
        app.toast
            .as_ref()
            .map(|toast| pill(&toast.text, if toast.error { RED } else { GREEN }))
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

// ----- overview -----------------------------------------------------------------

const FIELD: usize = 11;

fn draw_overview(frame: &mut Frame, area: Rect, app: &App) {
    let [top, bottom] = Layout::vertical([Constraint::Length(12), Constraint::Min(3)]).areas(area);
    let [core_area, right] =
        Layout::horizontal([Constraint::Percentage(52), Constraint::Percentage(48)]).areas(top);
    let [traffic_area, clash_area] =
        Layout::vertical([Constraint::Length(7), Constraint::Min(0)]).areas(right);

    draw_core_panel(frame, core_area, app);
    draw_traffic_panel(frame, traffic_area, app);
    draw_clash_panel(frame, clash_area, app);

    let block = panel(&fl!("tui-panel-recent-logs"), false);
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
                .unwrap_or_else(|| fl!("tui-waiting-daemon")),
            Style::new().fg(RED),
        ))],
        Some(status) => {
            let mut state = vec![state_pill(status.state)];
            if let (Some(pid), Some(started)) = (status.pid, status.started_at) {
                state.push(dim("  "));
                state.push(dim(fl!(
                    "state-pid-uptime",
                    pid = pid.to_string(),
                    uptime = fmt_duration(now.saturating_sub(started))
                )));
            }
            if let Some(at) = status.next_restart_at {
                state.push(Span::raw("  "));
                state.push(Span::styled(
                    fl!("tui-restart-in", seconds = at.saturating_sub(now)),
                    Style::new().fg(YELLOW),
                ));
            }
            let mut core = vec![Span::styled(
                status
                    .core_version
                    .clone()
                    .unwrap_or_else(|| fl!("tui-core-not-installed")),
                Style::new().fg(TEXT).bold(),
            )];
            if let Some(active) = &status.active_core {
                core.push(dim("  "));
                core.push(Span::styled(active.source_label(), Style::new().fg(ACCENT)));
                core.push(dim("  "));
                core.push(Span::styled(
                    variant_label(&active.variant),
                    Style::new().fg(BLUE),
                ));
            }
            if status.update_in_progress {
                core.push(Span::raw("  "));
                core.push(Span::styled(
                    fl!("tui-downloading"),
                    Style::new().fg(YELLOW),
                ));
            }
            let last_exit = status.last_exit.clone().unwrap_or_else(|| fl!("none"));
            let mut lines = vec![
                Line::from({
                    let mut line = vec![label(&fl!("field-state"), FIELD)];
                    line.extend(state);
                    line
                }),
                Line::from({
                    let mut line = vec![label(&fl!("field-core"), FIELD)];
                    line.extend(core);
                    line
                }),
                field(
                    &fl!("field-binary"),
                    FIELD,
                    Span::styled(status.binary.clone(), Style::new().fg(SUBTEXT)),
                ),
                Line::from(vec![
                    label(&fl!("field-restarts"), FIELD),
                    Span::raw(status.restarts.to_string()),
                    dim(format!("   {}  ", fl!("field-last-exit"))),
                    Span::styled(
                        last_exit,
                        Style::new().fg(if status.state == CoreState::Failed {
                            RED
                        } else {
                            SUBTEXT
                        }),
                    ),
                ]),
                field(
                    &fl!("field-daemon"),
                    FIELD,
                    dim(fl!(
                        "tui-daemon-detail",
                        version = status.daemon_version.clone(),
                        uptime = fmt_duration(now.saturating_sub(status.daemon_started_at)),
                        socket = app.socket()
                    )),
                ),
            ];
            for component in &status.components {
                let mut line = vec![
                    label(component.component.title(), FIELD),
                    component_badge(component),
                ];
                if let Some(url) = component.url.as_ref().filter(|_| component.pid.is_some()) {
                    line.push(dim(format!("  {url}")));
                }
                lines.push(Line::from(line));
            }
            lines
        }
    };
    frame.render_widget(Paragraph::new(lines).block(panel("sing-box", false)), area);
}

fn draw_traffic_panel(frame: &mut Frame, area: Rect, app: &App) {
    let block = panel(&fl!("tui-panel-traffic"), false);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let [up_label, up_spark, down_label, down_spark] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(2),
    ])
    .areas(inner);
    let t = &app.traffic;
    let width = inner.width as usize;
    let label = |arrow: &str, color: Color, speed: u64, total: u64| {
        let speed = format!("{arrow} {:>12}", fmt_speed(speed));
        let total = format!("   {}", fl!("tui-traffic-total", bytes = fmt_bytes(total)));
        let mut spans = vec![Span::styled(speed.clone(), Style::new().fg(color).bold())];
        // Drop the total rather than clip it in narrow terminals.
        if text_width(&speed) + text_width(&total) <= width {
            spans.push(dim(total));
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

const CLASH_FIELD: usize = 8;

fn draw_clash_panel(frame: &mut Frame, area: Rect, app: &App) {
    let api = app.status.as_ref().and_then(|s| s.clash_api.as_ref());
    let mut lines = vec![field(
        "API",
        CLASH_FIELD,
        match api {
            Some(api) => Span::styled(api.url.clone(), Style::new().fg(SUBTEXT)),
            None => Span::styled(fl!("clash-api-not-configured"), Style::new().fg(YELLOW)),
        },
    )];
    if let Some(err) = &app.clash_error {
        lines.push(field(
            &fl!("field-error"),
            CLASH_FIELD,
            Span::styled(first_line(err), Style::new().fg(RED)),
        ));
    } else if api.is_some() {
        let mode = app
            .configs
            .as_ref()
            .map_or_else(|| fl!("unknown"), |c| c.mode.to_lowercase());
        lines.push(field(&fl!("field-mode"), CLASH_FIELD, chip(mode, SKY)));
        lines.push(Line::from(vec![
            label(&fl!("field-connections"), CLASH_FIELD),
            Span::styled(
                app.connections.len().to_string(),
                Style::new().fg(TEXT).bold(),
            ),
            dim(format!("   {}  ", fl!("field-memory"))),
            Span::raw(fmt_bytes(app.traffic.memory)),
        ]));
    }
    frame.render_widget(Paragraph::new(lines).block(panel("Clash API", false)), area);
}

// ----- proxies ------------------------------------------------------------------

fn delay_span(app: &App, name: &str) -> Span<'static> {
    if app.testing.contains(name) {
        return Span::styled(
            format!(
                "{} {}",
                SPINNER[app.frame % SPINNER.len()],
                fl!("tui-testing")
            ),
            Style::new().fg(YELLOW),
        );
    }
    match app.delay_of(name) {
        None => dim("-"),
        Some(Err(_)) => Span::styled(fl!("tui-delay-timeout"), Style::new().fg(RED)),
        Some(Ok(ms)) => Span::styled(format!("{ms} ms"), Style::new().fg(delay_color(ms))),
    }
}

fn draw_proxies(frame: &mut Frame, area: Rect, app: &mut App) {
    let [groups_area, members_area] =
        Layout::horizontal([Constraint::Percentage(34), Constraint::Percentage(66)]).areas(area);

    if let Some(err) = app.clash_error.as_ref().filter(|_| app.groups.is_empty()) {
        let text = Paragraph::new(first_line(err))
            .fg(RED)
            .block(panel(&fl!("tui-panel-groups"), false));
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
                    Span::styled(name.clone(), Style::new().fg(TEXT).bold()),
                    Span::raw(" "),
                    dim(kind),
                ]),
                Line::from(vec![
                    dim("  → "),
                    Span::styled(now, Style::new().fg(ACCENT)),
                ]),
            ])
        })
        .collect();
    let groups_focused = app.focus == Focus::Groups;
    let list = List::new(items)
        .block(panel(
            &fl!("tui-panel-groups-count", count = app.groups.len()),
            groups_focused,
        ))
        .highlight_style(selected(groups_focused))
        .highlight_symbol(Span::styled(MARK, Style::new().fg(ACCENT)));
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
                    Span::styled("●", Style::new().fg(GREEN))
                } else {
                    Span::raw(" ")
                }),
                Cell::from(Span::styled(
                    member.clone(),
                    if active {
                        Style::new().fg(GREEN).bold()
                    } else {
                        Style::new()
                    },
                )),
                Cell::from(dim(kind)),
                Cell::from(delay_span(app, &member)),
            ])
        })
        .collect();
    let members_focused = app.focus == Focus::Members;
    let hint = if selectable {
        format!("⏎ {}  t {}", fl!("key-select"), fl!("key-test"))
    } else {
        format!("t {}", fl!("key-test"))
    };
    let table = Table::new(
        rows,
        [
            Constraint::Length(1),
            Constraint::Fill(1),
            Constraint::Length(14),
            Constraint::Length(9),
        ],
    )
    .header(
        Row::new([
            String::new(),
            fl!("col-name"),
            fl!("col-type"),
            fl!("col-delay"),
        ])
        .style(header_row()),
    )
    .block(
        panel(&group_name, members_focused)
            .title_top(Line::from(dim(format!(" {hint} "))).right_aligned()),
    )
    .row_highlight_style(selected(members_focused))
    .highlight_symbol(Span::styled(MARK, Style::new().fg(ACCENT)));
    frame.render_stateful_widget(table, members_area, &mut app.member_state);
}

// ----- connections --------------------------------------------------------------

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
    let block = panel(
        &fl!("tui-panel-connections", count = app.connections.len()),
        true,
    )
    .title_top(
        Line::from(vec![
            Span::styled(
                format!(" ↑ {} ", fmt_speed(app.traffic.up_speed)),
                Style::new().fg(UP),
            ),
            Span::styled(
                format!("↓ {} ", fmt_speed(app.traffic.down_speed)),
                Style::new().fg(DOWN),
            ),
        ])
        .right_aligned(),
    );
    if let Some(err) = &app.clash_error {
        frame.render_widget(Paragraph::new(first_line(err)).fg(RED).block(block), area);
        return;
    }
    let rows: Vec<Row> = app
        .connections
        .iter()
        .map(|c| {
            Row::new(vec![
                Cell::from(Span::styled(c.target(), Style::new().fg(TEXT))),
                Cell::from(dim(c.metadata.network.clone())),
                // sing-box lists the final node first; show group → node instead.
                Cell::from(Span::styled(
                    c.chains
                        .iter()
                        .rev()
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(" → "),
                    Style::new().fg(ACCENT),
                )),
                Cell::from(dim(c.rule.clone())),
                Cell::from(Span::styled(fmt_bytes(c.upload), Style::new().fg(UP))),
                Cell::from(Span::styled(fmt_bytes(c.download), Style::new().fg(DOWN))),
                Cell::from(dim(age(&c.start))),
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
        Row::new([
            fl!("col-destination"),
            fl!("col-network"),
            fl!("col-chain"),
            fl!("col-rule"),
            fl!("col-upload"),
            fl!("col-download"),
            fl!("col-age"),
        ])
        .style(header_row()),
    )
    .block(block)
    .row_highlight_style(selected(true))
    .highlight_symbol(Span::styled(MARK, Style::new().fg(ACCENT)));
    frame.render_stateful_widget(table, area, &mut app.conn_state);
}

// ----- logs -----------------------------------------------------------------------

const LEVELS: [(&str, Color); 7] = [
    ("PANIC", RED),
    ("FATAL", RED),
    ("ERROR", RED),
    ("WARN", YELLOW),
    ("INFO", GREEN),
    ("DEBUG", BLUE),
    ("TRACE", DIM),
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
            dim(fmt_clock(entry.ts)),
            Span::styled(
                format!(" {} ", entry.source.label()),
                Style::new().fg(ACCENT).bold(),
            ),
            Span::raw(line.clone()),
        ]),
        LogSource::SubStore | LogSource::HttpMeta => Line::from(vec![
            dim(fmt_clock(entry.ts)),
            Span::styled(
                format!(" {} ", entry.source.label()),
                Style::new().fg(SKY).bold(),
            ),
            Span::raw(line.clone()),
        ]),
        LogSource::Core => match find_level(line) {
            Some((start, end, color)) => Line::from(vec![
                dim(line[..start].to_owned()),
                Span::styled(line[start..end].to_owned(), Style::new().fg(color).bold()),
                Span::raw(line[end..].to_owned()),
            ]),
            None => Line::raw(line.clone()),
        },
    }
}

fn draw_logs(frame: &mut Frame, area: Rect, app: &App) {
    let follow = if app.log_scroll == 0 {
        Span::styled(
            format!(" ● {} ", fl!("tui-logs-following")),
            Style::new().fg(GREEN),
        )
    } else {
        Span::styled(
            format!(" {} ", fl!("tui-logs-scrolled", lines = app.log_scroll)),
            Style::new().fg(YELLOW),
        )
    };
    let mut status = vec![follow];
    if !app.logs_connected {
        status.push(Span::styled(
            format!("{} ", fl!("tui-logs-disconnected")),
            Style::new().fg(RED),
        ));
    }
    let block = panel(&fl!("tui-panel-logs", count = app.logs.len()), true)
        .title_top(Line::from(status).right_aligned());
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let height = inner.height as usize;
    let end = app.logs.len().saturating_sub(app.log_scroll);
    let start = end.saturating_sub(height);
    let lines: Vec<Line> = app.logs.range(start..end).map(log_line).collect();
    frame.render_widget(Paragraph::new(lines), inner);
}

// ----- Sub-Store --------------------------------------------------------------------

fn component_badge(c: &ComponentStatus) -> Span<'static> {
    if let Some(busy) = c.busy_label() {
        pill(busy.to_uppercase(), YELLOW)
    } else if !c.enabled {
        chip(fl!("state-disabled"), DIM)
    } else {
        pill(c.state.label(), state_color(c.state))
    }
}

fn draw_sub_store(frame: &mut Frame, area: Rect, app: &mut App) {
    let [components_area, entries_area] =
        Layout::vertical([Constraint::Length(6), Constraint::Min(3)]).areas(area);
    let now = now_unix();

    let rows: Vec<Row> = Component::ALL
        .iter()
        .map(|component| match app.component(*component) {
            None => Row::new(vec![
                Cell::from(component.title()),
                Cell::from(dim(fl!("unknown"))),
            ]),
            Some(c) => {
                let uptime = c
                    .started_at
                    .filter(|_| c.pid.is_some())
                    .map(|s| fl!("tui-uptime", uptime = fmt_duration(now.saturating_sub(s))))
                    .unwrap_or_else(|| c.last_exit.clone().unwrap_or_default());
                let versions = join_list(
                    &c.versions
                        .iter()
                        .map(|(k, v)| format!("{} {v}", version_label(k)))
                        .collect::<Vec<_>>(),
                );
                Row::new(vec![
                    Cell::from(Span::styled(
                        component.title(),
                        Style::new().fg(TEXT).bold(),
                    )),
                    Cell::from(component_badge(c)),
                    Cell::from(dim(uptime)),
                    Cell::from(dim(versions)),
                    Cell::from(Span::styled(
                        c.url.clone().unwrap_or_default(),
                        Style::new().fg(SUBTEXT),
                    )),
                ])
            }
        })
        .collect();
    let focused = app.store_focus == StoreFocus::Components;
    let table = Table::new(
        rows,
        [
            Constraint::Length(10),
            Constraint::Length(18),
            Constraint::Length(14),
            Constraint::Fill(2),
            Constraint::Fill(3),
        ],
    )
    .header(
        Row::new([
            fl!("col-component"),
            fl!("col-state"),
            String::new(),
            fl!("col-versions"),
            fl!("col-url"),
        ])
        .style(header_row()),
    )
    .block(
        panel(&fl!("tui-panel-components"), focused)
            .title_top(Line::from(dim(format!(" ⏎ {} ", fl!("key-actions")))).right_aligned()),
    )
    // A background highlight would hide the state pills; bold only.
    .row_highlight_style(Style::new().add_modifier(ratatui::style::Modifier::BOLD))
    .highlight_symbol(Span::styled(MARK, Style::new().fg(ACCENT)));
    frame.render_stateful_widget(table, components_area, &mut app.comp_state);

    let focused = app.store_focus == StoreFocus::Entries;
    let running = app
        .component(Component::SubStore)
        .is_some_and(|c| c.state == CoreState::Running);
    let title = match &app.sub_store {
        Some(overview) => fl!(
            "tui-panel-subscriptions-version",
            version = overview.version.clone()
        ),
        None => fl!("tui-panel-subscriptions"),
    };
    let hint = format!(
        " y {}  p {} ",
        fl!("key-copy-url"),
        fl!("key-provider-snippet")
    );
    let block = panel(&title, focused).title_top(Line::from(dim(hint)).right_aligned());
    let message = if !app
        .component(Component::SubStore)
        .is_some_and(|c| c.enabled)
    {
        Some(fl!("tui-sub-store-disabled"))
    } else if !running {
        Some(fl!("tui-sub-store-stopped"))
    } else if let Some(err) = &app.sub_store_error {
        Some(first_line(err))
    } else if app.sub_store.as_ref().is_some_and(|o| o.entries.is_empty()) {
        Some(fl!("tui-no-subscriptions"))
    } else if app.sub_store.is_none() {
        Some(fl!("tui-loading"))
    } else {
        None
    };
    if let Some(message) = message {
        frame.render_widget(
            Paragraph::new(message)
                .fg(DIM)
                .wrap(Wrap { trim: true })
                .block(block),
            entries_area,
        );
        return;
    }
    let rows: Vec<Row> = app
        .sub_store
        .as_ref()
        .map(|o| o.entries.as_slice())
        .unwrap_or_default()
        .iter()
        .map(|e| {
            let name = if e.display_name.is_empty() || e.display_name == e.name {
                e.name.clone()
            } else {
                format!("{} ({})", e.name, e.display_name)
            };
            Row::new(vec![
                Cell::from(chip(e.kind.label(), ACCENT)),
                Cell::from(Span::styled(name, Style::new().fg(TEXT))),
                Cell::from(dim(e.detail.clone())),
                Cell::from(Span::styled(
                    e.singbox_url.clone(),
                    Style::new().fg(SUBTEXT),
                )),
            ])
        })
        .collect();
    let table = Table::new(
        rows,
        [
            Constraint::Length(12),
            Constraint::Fill(1),
            Constraint::Fill(2),
            Constraint::Fill(3),
        ],
    )
    .header(
        Row::new([
            fl!("col-type"),
            fl!("col-name"),
            fl!("col-source"),
            fl!("col-singbox-url"),
        ])
        .style(header_row()),
    )
    .block(block)
    .row_highlight_style(selected(focused))
    .highlight_symbol(Span::styled(MARK, Style::new().fg(ACCENT)));
    frame.render_stateful_widget(table, entries_area, &mut app.entry_state);
}

// ----- popups -------------------------------------------------------------------

fn popup_area(frame: &Frame, width: u16, height: u16) -> Rect {
    let area = frame.area();
    area.centered(
        Constraint::Length(width.min(area.width)),
        Constraint::Length(height.min(area.height)),
    )
}

/// Rows `lines` take inside a bordered, padded popup `width` columns wide.
fn wrapped_rows(lines: &[Line], width: u16) -> u16 {
    let inner = usize::from(width.saturating_sub(4)).max(1);
    lines
        .iter()
        .map(|l| l.width().max(1).div_ceil(inner))
        .sum::<usize>() as u16
}

/// A section title (empty key) or a `key  description` row.
fn help_lines(rows: &[(&'static str, String)]) -> Vec<Line<'static>> {
    rows.iter()
        .map(|(k, desc)| {
            if k.is_empty() && desc.is_empty() {
                Line::raw("")
            } else if k.is_empty() {
                Line::from(Span::styled(desc.clone(), Style::new().fg(ACCENT).bold()))
            } else {
                Line::from(vec![
                    Span::styled(format!("  {k:<13}"), Style::new().fg(TEXT).bold()),
                    dim(desc.clone()),
                ])
            }
        })
        .collect()
}

fn draw_help(frame: &mut Frame) {
    let blank = || ("", String::new());
    let left = help_lines(&[
        ("", fl!("help-global")),
        ("1-6 / Tab", fl!("help-switch-tab")),
        ("s / x / r", fl!("help-start-stop-restart")),
        ("R", fl!("help-reload")),
        ("c", fl!("help-check")),
        ("u", fl!("help-update")),
        ("m", fl!("help-mode")),
        ("q / Ctrl-C", fl!("help-quit")),
        blank(),
        ("", fl!("help-proxies")),
        ("←→ ↑↓", fl!("help-groups-nodes")),
        ("Enter", fl!("help-select-node")),
        ("t / T", fl!("help-delay-test")),
        blank(),
        ("", fl!("help-connections-logs")),
        ("d / D", fl!("help-close-connections")),
        ("PgUp/Dn End", fl!("help-scroll")),
    ]);
    let right = help_lines(&[
        ("", "Sub-Store".to_owned()),
        ("←→", fl!("help-store-panes")),
        ("Enter", fl!("help-store-enter")),
        ("y / w / p", fl!("help-store-copy")),
        blank(),
        ("", fl!("help-core")),
        ("←→", fl!("help-core-panes")),
        ("Enter", fl!("help-core-switch")),
        ("v / V", fl!("help-core-variant")),
        ("i", fl!("help-core-download")),
        ("d", fl!("help-core-delete")),
        ("p / n / f", fl!("help-core-list")),
        ("a", fl!("help-core-add-source")),
        ("I", fl!("help-core-import")),
    ]);
    let height = left.len().max(right.len()) as u16 + 2;
    let area = popup_area(frame, 104, height);
    frame.render_widget(Clear, area);
    let block = panel(&fl!("help-title"), true)
        .title_top(Line::from(dim(format!(" {} ", fl!("help-close-hint")))).right_aligned());
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let [l, r] =
        Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)]).areas(inner);
    frame.render_widget(Paragraph::new(left), l);
    frame.render_widget(Paragraph::new(right), r);
}

fn draw_confirm(frame: &mut Frame, message: &str) {
    let mut lines: Vec<Line> = Vec::new();
    for (i, line) in message.lines().enumerate() {
        lines.push(if i == 0 {
            Line::from(Span::styled(line.to_owned(), Style::new().fg(TEXT).bold()))
        } else {
            Line::from(Span::styled(line.to_owned(), Style::new().fg(SUBTEXT)))
        });
    }
    lines.push(Line::raw(""));
    lines.push(Line::from(vec![
        key("y"),
        dim(format!(" {}    ", fl!("key-confirm"))),
        key("n"),
        dim(format!(" {}", fl!("key-cancel"))),
    ]));
    let width: u16 = 72;
    let height = wrapped_rows(&lines, width) + 4;
    let text = Text::from(lines);
    let area = popup_area(frame, width, height);
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(text)
            .alignment(Alignment::Center)
            .wrap(Wrap { trim: false })
            .block(
                panel(&fl!("tui-panel-confirm"), true)
                    .padding(ratatui::widgets::Padding::uniform(1)),
            ),
        area,
    );
}

fn draw_message(frame: &mut Frame, title: &str, body: &str, error: bool, copyable: bool) {
    let area = frame.area();
    let width = (area.width * 4 / 5).max(40);
    let height = (body.lines().count() as u16 + 4)
        .min(area.height * 4 / 5)
        .max(5);
    let rect = popup_area(frame, width, height);
    frame.render_widget(Clear, rect);
    let color = if error { RED } else { GREEN };
    let hint = if copyable {
        fl!("tui-hint-copy-close")
    } else {
        fl!("tui-hint-close")
    };
    frame.render_widget(
        Paragraph::new(body.to_owned())
            .wrap(Wrap { trim: false })
            .block(
                panel(title, true)
                    .border_style(Style::new().fg(color))
                    .title_top(Line::from(dim(format!(" {hint} "))).right_aligned()),
            ),
        rect,
    );
}

fn draw_input(frame: &mut Frame, title: &str, hint: &str, value: &str, purpose: InputPurpose) {
    let placeholder = match purpose {
        InputPurpose::AddSource => "owner/repo",
        InputPurpose::ImportCore => "/path/to/sing-box  [sha256]",
    };
    let input = if value.is_empty() {
        Line::from(vec![
            Span::styled("❯ ", Style::new().fg(ACCENT)),
            Span::styled("█", Style::new().fg(ACCENT)),
            dim(format!(" {placeholder}")),
        ])
    } else {
        Line::from(vec![
            Span::styled("❯ ", Style::new().fg(ACCENT)),
            Span::styled(value.to_owned(), Style::new().fg(TEXT).bg(SURFACE)),
            Span::styled("█", Style::new().fg(ACCENT)),
        ])
    };
    let lines = vec![
        Line::from(dim(hint.to_owned())),
        Line::raw(""),
        input,
        Line::raw(""),
        Line::from(vec![
            key("⏎"),
            dim(format!(" {}    ", fl!("key-submit"))),
            key("Esc"),
            dim(format!(" {}    ", fl!("key-cancel"))),
            key("^U"),
            dim(format!(" {}", fl!("key-clear"))),
        ]),
    ];
    let width: u16 = 80;
    let height = wrapped_rows(&lines, width) + 2;
    let area = popup_area(frame, width, height);
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .block(panel(title, true)),
        area,
    );
}

fn draw_setup(frame: &mut Frame, sub_store: Option<bool>) {
    let (name, about, question) = match sub_store {
        None => (
            "Sub-Store",
            fl!("setup-sub-store-about"),
            fl!("ask-sub-store"),
        ),
        Some(_) => (
            "http-meta",
            fl!("setup-http-meta-about"),
            fl!("ask-http-meta"),
        ),
    };
    let mut lines = vec![
        Line::from(dim(fl!("setup-intro"))),
        Line::raw(""),
        Line::from(Span::styled(name, Style::new().fg(ACCENT).bold())),
    ];
    lines.extend(
        about
            .lines()
            .map(|t| Line::from(Span::styled(t.to_owned(), Style::new().fg(SUBTEXT)))),
    );
    if let Some(answer) = sub_store {
        lines.push(Line::raw(""));
        lines.push(Line::from(dim(if answer {
            fl!("setup-sub-store-yes")
        } else {
            fl!("setup-sub-store-no")
        })));
    }
    lines.push(Line::raw(""));
    lines.push(Line::from(vec![
        Span::styled(question, Style::new().fg(TEXT).bold()),
        Span::raw("   "),
        key("y"),
        dim(format!(" {}  ", fl!("key-yes"))),
        key("n"),
        dim(format!(" {}  ", fl!("key-no"))),
        key("Esc"),
        dim(format!(" {}", fl!("key-ask-later"))),
    ]));
    let width: u16 = 76;
    let height = wrapped_rows(&lines, width) + 2;
    let area = popup_area(frame, width, height);
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .block(panel(&fl!("setup-title"), true)),
        area,
    );
}

fn draw_menu(
    frame: &mut Frame,
    component: Component,
    actions: &[ComponentAction],
    selected_index: usize,
) {
    let items: Vec<ListItem> = actions
        .iter()
        .map(|action| {
            let label = match action {
                ComponentAction::Start => fl!("menu-start"),
                ComponentAction::Stop => fl!("menu-stop"),
                ComponentAction::Restart => fl!("menu-restart"),
                ComponentAction::Enable => fl!("menu-enable"),
                ComponentAction::Disable => fl!("menu-disable"),
                ComponentAction::Update => fl!("menu-update"),
            };
            ListItem::new(format!(" {label}"))
        })
        .collect();
    let area = popup_area(frame, 48, actions.len() as u16 + 2);
    frame.render_widget(Clear, area);
    let mut state = ratatui::widgets::ListState::default().with_selected(Some(selected_index));
    frame.render_stateful_widget(
        List::new(items)
            .block(panel(component.title(), true))
            .highlight_style(selected(true))
            .highlight_symbol(Span::styled(MARK, Style::new().fg(ACCENT))),
        area,
        &mut state,
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
        assert_eq!((start, end, color), (0, 10, YELLOW));
        let line = "+0800 2026-10-05 12:00:00 ERROR router: oops";
        let (start, end, color) = find_level(line).unwrap();
        assert_eq!((&line[start..end], color), ("ERROR", RED));
        assert!(find_level("plain text").is_none());
    }

    #[test]
    fn level_token_must_be_exact() {
        assert!(find_level("INFORMATION about something").is_none());
        assert_eq!(find_level("INFO[0000] started").unwrap(), (0, 10, GREEN));
    }
}
