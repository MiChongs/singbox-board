//! Rendering. Pure functions of [`App`] state.

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Margin, Rect};
use ratatui::style::{Color, Style, Stylize};
use ratatui::symbols::Marker;
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{
    Axis, Block, Cell, Chart, Clear, Dataset, GraphType, List, ListItem, Paragraph, Row, Table,
    Tabs, Wrap,
};

use super::app::{App, Focus, Popup, StoreFocus, Tab};
use super::connections as connections_view;
use super::containers as containers_view;
use super::core as core_view;
use super::popup::{Input, InputPurpose, Menu};
use super::profiles as profiles_view;
use super::theme::{
    ACCENT, BLUE, BORDER, CRUST, DIM, DOWN, GREEN, MANTLE, MARK, PEACH, RED, SKY, SPINNER, SUBTEXT,
    SURFACE2, TEAL, TEXT, UP, YELLOW, backdrop, card, chip, delay_color, dialog, dialog_keys, dim,
    field, header_row, key, label, panel, pill, scrollbar, selected, signal, state_color,
    state_dot,
};
use crate::i18n::fl;
use crate::protocol::{
    Component, ComponentStatus, CoreState, LogEntry, LogSource, variant_label, version_label,
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
        Tab::Connections => connections_view::draw(frame, body, app),
        Tab::Logs => draw_logs(frame, body, app),
        Tab::SubStore => draw_sub_store(frame, body, app),
        Tab::Core => core_view::draw(frame, body, app),
        Tab::Profiles => profiles_view::draw(frame, body, app),
        Tab::Containers => containers_view::draw(frame, body, app),
    }
    draw_footer(frame, footer, app);

    // Dialogs stand out from a faded screen; the live connection filter
    // keeps the table readable behind it instead.
    let live_filter = matches!(
        &app.popup,
        Some(Popup::Input(input)) if matches!(input.purpose, InputPurpose::ConnectionFilter(_))
    );
    if app.popup.is_some() && !live_filter {
        backdrop(frame);
    }
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
        Some(Popup::Menu(menu)) => draw_menu(frame, menu),
        Some(Popup::Input(input)) => draw_input(frame, input),
        Some(Popup::EditorHelp) => draw_editor_help(frame),
        Some(Popup::CodeHelp) => draw_code_help(frame),
        None => {}
    }
}

// ----- chrome ------------------------------------------------------------------

/// Spans of `segments` joined by thin separators.
fn join_segments(segments: &[(u8, Vec<Span<'static>>)]) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    for (i, (_, segment)) in segments.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(" │ ", Style::new().fg(SURFACE2)));
        }
        spans.extend(segment.iter().cloned());
    }
    spans
}

fn spans_width(spans: &[Span]) -> usize {
    spans.iter().map(Span::width).sum()
}

/// Drops the segments of the lowest priority until the rest fit `width`.
fn fit_segments(segments: &mut Vec<(u8, Vec<Span<'static>>)>, width: usize) {
    while !segments.is_empty() && spans_width(&join_segments(segments)) > width {
        let lowest = segments
            .iter()
            .enumerate()
            .min_by_key(|(_, (priority, _))| *priority)
            .map_or(0, |(i, _)| i);
        segments.remove(lowest);
    }
}

fn draw_header(frame: &mut Frame, area: Rect, app: &App) {
    frame.render_widget(Block::new().style(Style::new().fg(TEXT).bg(MANTLE)), area);
    let brand = Span::styled(
        " ◆ singbox-board ",
        Style::new().fg(CRUST).bg(ACCENT).bold(),
    );
    // Each segment has a priority; the least important go first when the
    // terminal is narrow.
    let mut segments: Vec<(u8, Vec<Span<'static>>)> = Vec::new();
    match (&app.status, &app.status_error) {
        (Some(status), _) => {
            segments.push((9, state_dot(status.state)));
            segments.push((
                8,
                vec![
                    dim("sing-box "),
                    Span::styled(
                        status
                            .core_version
                            .clone()
                            .unwrap_or_else(|| fl!("not-installed")),
                        Style::new().fg(TEXT).bold(),
                    ),
                ],
            ));
            if let Some(core) = &status.active_core {
                segments.push((
                    3,
                    vec![
                        Span::styled(core.source_label(), Style::new().fg(ACCENT)),
                        dim(" · "),
                        Span::styled(variant_label(&core.variant), Style::new().fg(BLUE)),
                    ],
                ));
            }
            if let Some(profile) = &status.active_profile {
                segments.push((
                    7,
                    vec![
                        dim(format!("{} ", fl!("field-profile"))),
                        Span::styled(profile.name.clone(), Style::new().fg(GREEN).bold()),
                    ],
                ));
            }
            if let Some(mode) = app
                .configs
                .as_ref()
                .map(|c| c.mode.as_str())
                .filter(|m| !m.is_empty())
            {
                segments.push((
                    6,
                    vec![
                        dim(format!("{} ", fl!("field-mode"))),
                        Span::styled(mode.to_lowercase(), Style::new().fg(SKY).bold()),
                    ],
                ));
            }
            if let Some(containers) = status.containers.filter(|c| c.total > 0) {
                segments.push((
                    2,
                    vec![Span::styled(
                        fl!(
                            "tui-header-containers",
                            running = containers.running,
                            total = containers.total
                        ),
                        Style::new().fg(if containers.running > 0 { TEAL } else { DIM }),
                    )],
                ));
            }
            if let Some(started) = status.started_at {
                segments.push((
                    4,
                    vec![dim(fl!(
                        "tui-uptime",
                        uptime = fmt_duration(now_unix().saturating_sub(started))
                    ))],
                ));
            }
        }
        (None, Some(err)) => segments.push((
            9,
            vec![Span::styled(
                format!("✕ {}", first_line(err)),
                Style::new().fg(RED),
            )],
        )),
        (None, None) => segments.push((
            9,
            vec![Span::styled(
                format!(
                    "{} {}",
                    SPINNER[app.frame % SPINNER.len()],
                    fl!("tui-connecting")
                ),
                Style::new().fg(YELLOW),
            )],
        )),
    }
    let room = usize::from(area.width).saturating_sub(brand.width() + 2);
    fit_segments(&mut segments, room);
    let mut left = vec![brand, Span::raw("  ")];
    left.extend(join_segments(&segments));
    let left = Line::from(left);

    // The daemon on the right, only when it fits beside the rest.
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
    if right_width > 0 && left.width() as u16 + right_width + 2 < area.width {
        let [left_area, right_area] =
            Layout::horizontal([Constraint::Min(0), Constraint::Length(right_width)]).areas(area);
        frame.render_widget(Paragraph::new(left), left_area);
        frame.render_widget(Paragraph::new(right.unwrap_or_default()), right_area);
    } else {
        frame.render_widget(Paragraph::new(left), area);
    }
}

/// A count shown beside a tab's title.
fn tab_badge(app: &App, tab: Tab) -> Option<String> {
    match tab {
        Tab::Connections if app.connections.loaded => Some(app.connections.live_len().to_string()),
        Tab::Containers => app
            .status
            .as_ref()
            .and_then(|s| s.containers)
            .filter(|c| c.total > 0)
            .map(|c| format!("{}/{}", c.running, c.total)),
        _ => None,
    }
}

fn draw_tabs(frame: &mut Frame, area: Rect, app: &App) {
    let title = |i: usize, tab: Tab, short: bool| {
        let mut spans = vec![Span::styled(format!(" {} ", i + 1), Style::new().fg(DIM))];
        if !short || tab == app.tab {
            spans.push(Span::styled(tab.title(), Style::new().fg(SUBTEXT)));
            if let Some(badge) = tab_badge(app, tab) {
                spans.push(Span::styled(format!(" {badge}"), Style::new().fg(DIM)));
            }
            spans.push(Span::raw(" "));
        }
        Line::from(spans)
    };
    let titles = |short: bool| -> Vec<Line<'static>> {
        Tab::ALL
            .iter()
            .enumerate()
            .map(|(i, tab)| title(i, *tab, short))
            .collect()
    };
    // Only the open tab keeps its name when all of them do not fit.
    let mut lines = titles(false);
    let width: usize = lines.iter().map(|l| l.width() + 1).sum();
    if width > usize::from(area.width.saturating_sub(2)) {
        lines = titles(true);
    }
    let tabs = Tabs::new(lines)
        .select(app.tab.index())
        .padding("", "")
        .divider(Span::styled(" ", Style::new()))
        .highlight_style(Style::new().fg(CRUST).bg(ACCENT).bold());
    frame.render_widget(tabs, area.inner(Margin::new(1, 0)));
}

fn draw_footer(frame: &mut Frame, area: Rect, app: &App) {
    frame.render_widget(Block::new().style(Style::new().fg(TEXT).bg(MANTLE)), area);
    let keys: Vec<(&str, String)> = match app.tab {
        Tab::Proxies => vec![
            ("←→", fl!("key-focus")),
            ("⏎", fl!("key-select")),
            ("t", fl!("key-test")),
            ("T", fl!("key-test-one")),
        ],
        Tab::Connections => connections_view::hints(app),
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
        Tab::Profiles => profiles_view::hints(app),
        Tab::Containers => containers_view::hints(app),
        Tab::Overview => Vec::new(),
    };
    // The editors take these keys themselves.
    let global = if (app.tab == Tab::Profiles && app.profiles.code.is_some())
        || (app.tab == Tab::Containers && app.containers.editor.is_some())
    {
        Vec::new()
    } else {
        vec![
            ("s", fl!("key-start")),
            ("x", fl!("key-stop")),
            ("r", fl!("key-restart")),
            ("R", fl!("key-reload")),
            ("u", fl!("key-update")),
            ("m", fl!("key-mode")),
            ("?", fl!("key-help")),
        ]
    };

    let right = if let Some((_, label)) = app.busy.last() {
        Some(Span::styled(
            format!(" {} {label} ", SPINNER[app.frame % SPINNER.len()]),
            Style::new().fg(CRUST).bg(YELLOW).bold(),
        ))
    } else {
        app.toast.as_ref().map(|toast| {
            if toast.error {
                pill(format!("✕ {}", toast.text), RED)
            } else {
                pill(format!("✓ {}", toast.text), GREEN)
            }
        })
    };
    let right_width = right.as_ref().map_or(0, |span| span.width());

    // As many hints as fit beside the toast, the tab's own first.
    let room = usize::from(area.width).saturating_sub(right_width + 1);
    let mut spans = vec![Span::raw(" ")];
    let mut used = 1;
    for (i, (k, label)) in keys.iter().chain(global.iter()).enumerate() {
        let mut item = Vec::new();
        if i == keys.len() && !keys.is_empty() {
            item.push(Span::styled("│  ", Style::new().fg(SURFACE2)));
        }
        item.push(key(k));
        item.push(Span::styled(
            format!(" {label}  "),
            Style::new().fg(SUBTEXT),
        ));
        let width = spans_width(&item);
        if used + width > room {
            break;
        }
        used += width;
        spans.extend(item);
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);

    if let Some(span) = right {
        let width = (right_width as u16).min(area.width);
        let rect = Rect {
            x: area.right() - width,
            width,
            ..area
        };
        frame.render_widget(Paragraph::new(span), rect);
    }
}

// ----- overview -----------------------------------------------------------------

const FIELD: usize = 11;

fn draw_overview(frame: &mut Frame, area: Rect, app: &App) {
    let cards = area.height >= 24;
    // Tall screens give the chart more rows and the facts some air.
    let spacious = area.height >= 36;
    let info = info_lines(app, !cards, spacious);
    let middle = (info.len() as u16 + 2)
        .max(if spacious { 14 } else { 9 })
        .min(area.height.saturating_sub(if cards { 4 } else { 0 }));
    let [cards_area, middle_area, logs_area] = Layout::vertical([
        Constraint::Length(if cards { 4 } else { 0 }),
        Constraint::Length(middle),
        Constraint::Min(0),
    ])
    .areas(area);
    if cards {
        draw_overview_cards(frame, cards_area, app);
    }
    if area.width >= 100 {
        let [info_area, chart_area] =
            Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)])
                .areas(middle_area);
        frame.render_widget(
            Paragraph::new(info).block(panel("sing-box", false)),
            info_area,
        );
        draw_traffic_chart(frame, chart_area, app);
    } else {
        frame.render_widget(
            Paragraph::new(info).block(panel("sing-box", false)),
            middle_area,
        );
    }

    if logs_area.height >= 3 {
        let block = panel(&fl!("tui-panel-recent-logs"), false);
        let inner = block.inner(logs_area);
        frame.render_widget(block, logs_area);
        let height = inner.height as usize;
        let start = app.logs.len().saturating_sub(height);
        let lines: Vec<Line> = app.logs.iter().skip(start).map(log_line).collect();
        frame.render_widget(Paragraph::new(lines), inner);
    }
}

fn draw_overview_cards(frame: &mut Frame, area: Rect, app: &App) {
    let [state_area, down_area, up_area, count_area] = Layout::horizontal([
        Constraint::Fill(1),
        Constraint::Fill(1),
        Constraint::Fill(1),
        Constraint::Fill(1),
    ])
    .areas(area);
    let now = now_unix();

    // sing-box: the state and how long it has been in it.
    let (color, lines) = match &app.status {
        None => (
            YELLOW,
            vec![
                Line::from(Span::styled(
                    format!(
                        "{} {}",
                        SPINNER[app.frame % SPINNER.len()],
                        fl!("tui-connecting")
                    ),
                    Style::new().fg(YELLOW).bold(),
                )),
                Line::from(dim(app
                    .status_error
                    .as_deref()
                    .map(first_line)
                    .unwrap_or_default())),
            ],
        ),
        Some(status) => {
            let mut first = state_dot(status.state);
            if let Some(pid) = status.pid {
                first.push(dim(format!("  PID {pid}")));
            }
            let second = if let Some(at) = status.next_restart_at {
                Line::from(Span::styled(
                    fl!("tui-restart-in", seconds = at.saturating_sub(now)),
                    Style::new().fg(YELLOW),
                ))
            } else if let Some(started) = status.started_at {
                Line::from(dim(fl!(
                    "tui-uptime",
                    uptime = fmt_duration(now.saturating_sub(started))
                )))
            } else {
                Line::from(dim(status.last_exit.clone().unwrap_or_default()))
            };
            (state_color(status.state), vec![Line::from(first), second])
        }
    };
    frame.render_widget(
        Paragraph::new(lines).block(card("sing-box", color)),
        state_area,
    );

    let t = &app.traffic;
    for (area, title, arrow, color, speed, total) in [
        (
            down_area,
            fl!("conn-card-download"),
            "↓",
            DOWN,
            t.down_speed,
            t.down_total,
        ),
        (
            up_area,
            fl!("conn-card-upload"),
            "↑",
            UP,
            t.up_speed,
            t.up_total,
        ),
    ] {
        frame.render_widget(
            Paragraph::new(vec![
                Line::from(Span::styled(
                    fmt_speed(speed),
                    Style::new().fg(color).bold(),
                )),
                Line::from(dim(fl!("tui-traffic-total", bytes = fmt_bytes(total)))),
            ])
            .block(card(format!("{arrow} {title}"), color)),
            area,
        );
    }
    connections_view::draw_count_card(frame, count_area, app);
}

/// The key/value lines of the sing-box panel; `state` adds the state
/// line the cards show otherwise, `spacious` blank lines between groups.
fn info_lines(app: &App, state: bool, spacious: bool) -> Vec<Line<'static>> {
    let now = now_unix();
    let Some(status) = &app.status else {
        return vec![Line::from(Span::styled(
            app.status_error
                .clone()
                .unwrap_or_else(|| fl!("tui-waiting-daemon")),
            Style::new().fg(RED),
        ))];
    };
    let mut lines = Vec::new();
    if state {
        let mut line = vec![label(&fl!("field-state"), FIELD)];
        line.extend(state_dot(status.state));
        if let (Some(pid), Some(started)) = (status.pid, status.started_at) {
            line.push(dim(format!(
                "  {}",
                fl!(
                    "state-pid-uptime",
                    pid = pid.to_string(),
                    uptime = fmt_duration(now.saturating_sub(started))
                )
            )));
        }
        lines.push(Line::from(line));
    }
    let mut core = vec![
        label(&fl!("field-core"), FIELD),
        Span::styled(
            status
                .core_version
                .clone()
                .unwrap_or_else(|| fl!("tui-core-not-installed")),
            Style::new().fg(TEXT).bold(),
        ),
    ];
    if let Some(active) = &status.active_core {
        core.push(Span::raw("  "));
        core.push(Span::styled(active.source_label(), Style::new().fg(ACCENT)));
        core.push(dim(" · "));
        core.push(Span::styled(
            variant_label(&active.variant),
            Style::new().fg(BLUE),
        ));
    }
    if status.update_in_progress {
        core.push(Span::raw("  "));
        core.push(pill(fl!("tui-downloading"), YELLOW));
    }
    lines.push(Line::from(core));
    lines.push(field(
        &fl!("field-profile"),
        FIELD,
        match &status.active_profile {
            Some(profile) => Span::styled(profile.name.clone(), Style::new().fg(GREEN).bold()),
            None => Span::styled(fl!("tui-profile-unmanaged"), Style::new().fg(YELLOW)),
        },
    ));
    lines.push(field(
        &fl!("field-binary"),
        FIELD,
        Span::styled(status.binary.clone(), Style::new().fg(SUBTEXT)),
    ));
    let last_exit = status.last_exit.clone().unwrap_or_else(|| fl!("none"));
    lines.push(Line::from(vec![
        label(&fl!("field-restarts"), FIELD),
        Span::styled(status.restarts.to_string(), Style::new().fg(TEXT)),
        dim(format!("   {}  ", fl!("field-last-exit"))),
        Span::styled(
            last_exit,
            Style::new().fg(if status.state == CoreState::Failed {
                RED
            } else {
                SUBTEXT
            }),
        ),
    ]));
    if spacious {
        lines.push(Line::raw(""));
    }
    lines.push(field(
        &fl!("field-daemon"),
        FIELD,
        dim(fl!(
            "tui-daemon-detail",
            version = status.daemon_version.clone(),
            uptime = fmt_duration(now.saturating_sub(status.daemon_started_at)),
            socket = app.socket()
        )),
    ));
    let mut clash = vec![label("Clash API", FIELD)];
    match (&status.clash_api, &app.clash_error) {
        (None, _) => clash.push(Span::styled(
            fl!("clash-api-not-configured"),
            Style::new().fg(YELLOW),
        )),
        (Some(_), Some(err)) => clash.push(Span::styled(first_line(err), Style::new().fg(RED))),
        (Some(api), None) => {
            clash.push(Span::styled(api.url.clone(), Style::new().fg(SUBTEXT)));
            if let Some(configs) = app.configs.as_ref().filter(|c| !c.mode.is_empty()) {
                clash.push(Span::raw("  "));
                clash.push(chip(configs.mode.to_lowercase(), SKY));
            }
        }
    }
    lines.push(Line::from(clash));
    if spacious && !status.components.is_empty() {
        lines.push(Line::raw(""));
    }
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

/// A round number at or above `value` for the top of the chart's scale.
fn nice_ceiling(value: u64) -> u64 {
    let mut unit = 1u64;
    while value > unit.saturating_mul(1000) {
        unit = unit.saturating_mul(1024);
    }
    [1, 2, 5, 10, 20, 50, 100, 200, 500, 1000]
        .iter()
        .map(|step| unit.saturating_mul(*step))
        .find(|&top| top >= value)
        .unwrap_or(value)
}

fn draw_traffic_chart(frame: &mut Frame, area: Rect, app: &App) {
    let t = &app.traffic;
    let legend = Line::from(vec![
        Span::raw(" "),
        Span::styled("━ ", Style::new().fg(DOWN)),
        Span::styled(
            format!("↓ {}", fmt_speed(t.down_speed)),
            Style::new().fg(DOWN).bold(),
        ),
        Span::raw("  "),
        Span::styled("━ ", Style::new().fg(UP)),
        Span::styled(
            format!("↑ {}", fmt_speed(t.up_speed)),
            Style::new().fg(UP).bold(),
        ),
        Span::raw(" "),
    ])
    .right_aligned();
    let block = panel(&fl!("tui-panel-traffic"), false).title_top(legend);
    let samples = t.down_history.len().min(t.up_history.len());
    if samples < 2 {
        let inner = block.inner(area);
        frame.render_widget(block, area);
        frame.render_widget(
            Paragraph::new(dim(fl!("tui-chart-collecting"))).alignment(Alignment::Center),
            inner.inner(Margin::new(0, inner.height / 2)),
        );
        return;
    }
    // One sample a second; the newest at the right edge.
    let window = super::app::HISTORY_POINTS as f64;
    let points = |history: &std::collections::VecDeque<u64>| -> Vec<(f64, f64)> {
        let len = history.len();
        history
            .iter()
            .enumerate()
            .map(|(i, v)| ((i + 1) as f64 - len as f64, *v as f64))
            .collect()
    };
    let down = points(&t.down_history);
    let up = points(&t.up_history);
    let peak = t
        .down_history
        .iter()
        .chain(t.up_history.iter())
        .copied()
        .max()
        .unwrap_or(0);
    let top = nice_ceiling(peak.max(1024));
    let axis = Style::new().fg(BORDER);
    let tick = |text: String| Span::styled(text, Style::new().fg(DIM));
    let window_secs = super::app::HISTORY_POINTS as u64;
    let chart = Chart::new(vec![
        Dataset::default()
            .marker(Marker::Braille)
            .graph_type(GraphType::Area)
            .style(Style::new().fg(DOWN))
            .data(&down),
        Dataset::default()
            .marker(Marker::Braille)
            .graph_type(GraphType::Line)
            .style(Style::new().fg(UP))
            .data(&up),
    ])
    .block(block)
    .legend_position(None)
    .x_axis(
        Axis::default()
            .style(axis)
            .bounds([1.0 - window, 0.0])
            .labels([
                tick(format!("-{}", fmt_duration(window_secs))),
                tick(format!("-{}", fmt_duration(window_secs / 2))),
                tick(fl!("tui-chart-now")),
            ]),
    )
    .y_axis(
        Axis::default()
            .style(axis)
            .bounds([0.0, top as f64])
            .labels([
                tick("0".to_owned()),
                tick(fmt_speed(top / 2)),
                tick(fmt_speed(top)),
            ]),
    );
    frame.render_widget(chart, area);
}

// ----- proxies ------------------------------------------------------------------

/// Colours of the outbound types that group others.
fn kind_color(kind: &str) -> Color {
    match kind.to_ascii_lowercase().as_str() {
        "selector" => ACCENT,
        "urltest" => SKY,
        "fallback" => PEACH,
        "loadbalance" => TEAL,
        "direct" => GREEN,
        "block" | "reject" => RED,
        _ => DIM,
    }
}

fn delay_spans(app: &App, name: &str) -> Vec<Span<'static>> {
    if app.testing.contains(name) {
        return vec![Span::styled(
            format!(
                "{} {}",
                SPINNER[app.frame % SPINNER.len()],
                fl!("tui-testing")
            ),
            Style::new().fg(YELLOW),
        )];
    }
    let delay = app.delay_of(name);
    let mut spans = signal(delay.as_ref().map(|d| d.as_ref().copied().map_err(drop)));
    spans.push(Span::raw(" "));
    spans.push(match delay {
        None => dim("-"),
        Some(Err(_)) => Span::styled(fl!("tui-delay-timeout"), Style::new().fg(RED)),
        Some(Ok(ms)) => Span::styled(format!("{ms} ms"), Style::new().fg(delay_color(ms))),
    });
    spans
}

fn draw_proxies(frame: &mut Frame, area: Rect, app: &mut App) {
    let [groups_area, members_area] =
        Layout::horizontal([Constraint::Percentage(34), Constraint::Percentage(66)]).areas(area);

    if let Some(err) = app.clash_error.as_ref().filter(|_| app.groups.is_empty()) {
        let text = Paragraph::new(first_line(err))
            .fg(RED)
            .wrap(Wrap { trim: true })
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
            let members = group.and_then(|g| g.all.as_ref()).map_or(0, Vec::len);
            let mut title = vec![
                Span::styled(name.clone(), Style::new().fg(TEXT).bold()),
                Span::raw("  "),
                Span::styled(kind.clone(), Style::new().fg(kind_color(&kind))),
            ];
            if members > 0 {
                title.push(dim(format!(" · {members}")));
            }
            let mut current = vec![
                Span::styled("  ↳ ", Style::new().fg(SURFACE2)),
                Span::styled(now.clone(), Style::new().fg(SUBTEXT)),
            ];
            if let Some(Ok(ms)) = app.delay_of(&now) {
                current.push(Span::styled(
                    format!("  {ms} ms"),
                    Style::new().fg(delay_color(ms)),
                ));
            }
            ListItem::new(vec![Line::from(title), Line::from(current)])
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
    scrollbar(
        frame,
        groups_area,
        (
            app.groups.len() * 2,
            app.group_state.offset() * 2,
            usize::from(groups_area.height.saturating_sub(2)),
        ),
        groups_focused,
    );

    let group_name = app.selected_group().unwrap_or_default().to_owned();
    let group = app.group(&group_name);
    let now = group.and_then(|g| g.now.clone()).unwrap_or_default();
    let kind = group.map(|g| g.kind.clone()).unwrap_or_default();
    let selectable = group.is_some_and(|g| g.is_selectable());
    let members = app.members();
    let rows: Vec<Row> = members
        .iter()
        .map(|member| {
            let active = *member == now;
            let member_kind = app
                .group(member)
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
                        Style::new().fg(TEXT)
                    },
                )),
                Cell::from(Span::styled(
                    member_kind.clone(),
                    Style::new().fg(if app.groups.contains(member) {
                        kind_color(&member_kind)
                    } else {
                        DIM
                    }),
                )),
                Cell::from(Line::from(delay_spans(app, member))),
            ])
        })
        .collect();
    let members_focused = app.focus == Focus::Members;
    let hint = if selectable {
        format!("⏎ {}  t {}", fl!("key-select"), fl!("key-test"))
    } else {
        format!("t {}", fl!("key-test"))
    };
    let mut block = panel(&group_name, members_focused)
        .title_top(Line::from(dim(format!(" {hint} "))).right_aligned());
    if !kind.is_empty() {
        block = block.title_bottom(Line::from(vec![
            Span::raw(" "),
            Span::styled(kind.clone(), Style::new().fg(kind_color(&kind))),
            dim(" → "),
            Span::styled(now.clone(), Style::new().fg(GREEN)),
            Span::raw(" "),
        ]));
    }
    if let Some(index) = app.member_state.selected().filter(|_| !members.is_empty()) {
        block = block.title_bottom(
            Line::from(dim(format!(" {} / {} ", index + 1, members.len()))).right_aligned(),
        );
    }
    let viewport = usize::from(block.inner(members_area).height.saturating_sub(1));
    let table = Table::new(
        rows,
        [
            Constraint::Length(1),
            Constraint::Fill(1),
            Constraint::Length(14),
            Constraint::Length(13),
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
    .block(block)
    .row_highlight_style(selected(members_focused))
    .highlight_symbol(Span::styled(MARK, Style::new().fg(ACCENT)));
    frame.render_stateful_widget(table, members_area, &mut app.member_state);
    scrollbar(
        frame,
        members_area,
        (members.len(), app.member_state.offset(), viewport),
        members_focused,
    );
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

/// Locates a `LEVEL:` token, as Sub-Store and http-meta write it:
/// `2026/10/7 02:50:36 [sub-store] INFO: message`.
fn find_colon_level(line: &str) -> Option<(usize, usize, Color)> {
    LEVELS.iter().find_map(|(level, color)| {
        let token = format!("{level}:");
        let index = if line.starts_with(&token) {
            0
        } else {
            line.find(&format!(" {token}"))? + 1
        };
        Some((index, index + token.len(), *color))
    })
}

/// `line` with its level token coloured and what comes before it dimmed.
fn leveled(line: &str, level: Option<(usize, usize, Color)>) -> Vec<Span<'static>> {
    match level {
        Some((start, end, color)) => vec![
            dim(line[..start].to_owned()),
            Span::styled(line[start..end].to_owned(), Style::new().fg(color).bold()),
            Span::raw(line[end..].to_owned()),
        ],
        None => vec![Span::raw(line.to_owned())],
    }
}

/// The tag of a log line's source.
fn source_tag(source: LogSource, color: Color) -> Span<'static> {
    Span::styled(
        format!(" {} ", source.label()),
        Style::new().fg(color).bold(),
    )
}

fn log_line(entry: &LogEntry) -> Line<'static> {
    let line = &entry.line;
    let mut spans = match entry.source {
        LogSource::Core => return Line::from(leveled(line, find_level(line))),
        LogSource::Daemon => vec![source_tag(entry.source, ACCENT), Span::raw(line.clone())],
        LogSource::SubStore | LogSource::HttpMeta => {
            let mut spans = vec![source_tag(entry.source, SKY)];
            spans.extend(leveled(line, find_colon_level(line)));
            spans
        }
        LogSource::Containers => {
            let mut spans = vec![source_tag(entry.source, TEAL)];
            spans.extend(leveled(line, find_level(line)));
            spans
        }
    };
    spans.insert(0, dim(fmt_clock(entry.ts)));
    Line::from(spans)
}

fn draw_logs(frame: &mut Frame, area: Rect, app: &App) {
    let follow = if app.log_scroll == 0 {
        Span::styled(
            format!(" ● {} ", fl!("tui-logs-following")),
            Style::new().fg(GREEN),
        )
    } else {
        Span::styled(
            format!(" ⏸ {} ", fl!("tui-logs-scrolled", lines = app.log_scroll)),
            Style::new().fg(YELLOW),
        )
    };
    let mut status = vec![follow];
    if !app.logs_connected {
        status.push(Span::styled(
            format!("✕ {} ", fl!("tui-logs-disconnected")),
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
    scrollbar(frame, area, (app.logs.len(), start, height), true);
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
                    .map(|s| fmt_duration(now.saturating_sub(s)))
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
                    Cell::from(Span::styled(versions, Style::new().fg(SUBTEXT))),
                    Cell::from(Span::styled(
                        c.url.clone().unwrap_or_default(),
                        Style::new().fg(BLUE),
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
            Constraint::Length(16),
            Constraint::Length(12),
            Constraint::Fill(2),
            Constraint::Fill(3),
        ],
    )
    .column_spacing(2)
    .header(
        Row::new([
            fl!("col-component"),
            fl!("col-state"),
            fl!("col-uptime"),
            fl!("col-versions"),
            fl!("col-url"),
        ])
        .style(header_row())
        .bottom_margin(1),
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
        Some(("○", DIM, fl!("tui-sub-store-disabled")))
    } else if !running {
        Some(("○", DIM, fl!("tui-sub-store-stopped")))
    } else if let Some(err) = &app.sub_store_error {
        Some(("✕", RED, err.clone()))
    } else if app.sub_store.as_ref().is_some_and(|o| o.entries.is_empty()) {
        Some(("○", DIM, fl!("tui-no-subscriptions")))
    } else if app.sub_store.is_none() {
        Some((
            SPINNER[app.frame % SPINNER.len()],
            YELLOW,
            fl!("tui-loading"),
        ))
    } else {
        None
    };
    if let Some((icon, color, message)) = message {
        let inner = block.inner(entries_area);
        frame.render_widget(block, entries_area);
        empty_state(frame, inner, (icon, color), &message, "");
        return;
    }
    let entries = app
        .sub_store
        .as_ref()
        .map(|o| o.entries.as_slice())
        .unwrap_or_default();
    let rows: Vec<Row> = entries
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
    let total = rows.len();
    let viewport = usize::from(block.inner(entries_area).height.saturating_sub(1));
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
    scrollbar(
        frame,
        entries_area,
        (total, app.entry_state.offset(), viewport),
        focused,
    );
}

/// A centred icon and message in an empty pane, with an optional hint.
pub(super) fn empty_state(
    frame: &mut Frame,
    area: Rect,
    (icon, color): (&str, Color),
    title: &str,
    hint: &str,
) {
    let mut lines = vec![Line::from(vec![
        Span::styled(format!("{icon}  "), Style::new().fg(color)),
        Span::styled(title.to_owned(), Style::new().fg(TEXT).bold()),
    ])];
    if !hint.is_empty() {
        lines.push(Line::raw(""));
        lines.extend(
            hint.lines()
                .map(|line| Line::from(Span::styled(line.to_owned(), Style::new().fg(DIM)))),
        );
    }
    let text = Text::from(lines);
    let rows = text.height() as u16 + 1;
    let [_, message] = Layout::vertical([
        Constraint::Length(area.height.saturating_sub(rows) / 2),
        Constraint::Min(0),
    ])
    .areas(area);
    frame.render_widget(
        Paragraph::new(text)
            .alignment(Alignment::Center)
            .wrap(Wrap { trim: true }),
        message,
    );
}

// ----- popups -------------------------------------------------------------------

fn popup_area(frame: &Frame, width: u16, height: u16) -> Rect {
    let area = frame.area();
    // Room for the shadow on the right and below.
    area.centered(
        Constraint::Length(width.min(area.width.saturating_sub(2))),
        Constraint::Length(height.min(area.height.saturating_sub(1))),
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
    help_lines_with(rows, 13)
}

fn help_lines_with(rows: &[(&'static str, String)], key_width: usize) -> Vec<Line<'static>> {
    rows.iter()
        .map(|(k, desc)| {
            if k.is_empty() && desc.is_empty() {
                Line::raw("")
            } else if k.is_empty() {
                Line::from(vec![
                    Span::styled(MARK, Style::new().fg(ACCENT)),
                    Span::styled(desc.clone(), Style::new().fg(ACCENT).bold()),
                ])
            } else {
                Line::from(vec![
                    Span::styled(format!("  {k:<key_width$} "), Style::new().fg(BLUE).bold()),
                    Span::styled(desc.clone(), Style::new().fg(SUBTEXT)),
                ])
            }
        })
        .collect()
}

/// A dialog of two columns of key help.
fn draw_help_columns(
    frame: &mut Frame,
    title: &str,
    (left, right): (Vec<Line<'static>>, Vec<Line<'static>>),
    width: u16,
    note: Option<String>,
) {
    let note_rows = if note.is_some() { 2 } else { 0 };
    let height = left.len().max(right.len()) as u16 + 2 + note_rows;
    let area = popup_area(frame, width, height);
    frame.render_widget(Clear, area);
    let block = dialog(title, ACCENT)
        .title_bottom(Line::from(dim(format!(" {} ", fl!("help-close-hint")))).right_aligned());
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let [columns, note_area] =
        Layout::vertical([Constraint::Min(0), Constraint::Length(note_rows)]).areas(inner);
    let [l, r] = Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)])
        .spacing(2)
        .areas(columns);
    frame.render_widget(Paragraph::new(left), l);
    frame.render_widget(Paragraph::new(right), r);
    if let Some(note) = note {
        frame.render_widget(
            Paragraph::new(dim(note)).wrap(Wrap { trim: true }),
            note_area,
        );
    }
}

fn draw_help(frame: &mut Frame) {
    let blank = || ("", String::new());
    let left = help_lines(&[
        ("", fl!("help-global")),
        ("1-8 / Tab", fl!("help-switch-tab")),
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
        ("", fl!("help-connections")),
        ("Enter / i", fl!("help-conn-details")),
        ("/ · Esc", fl!("help-conn-filter")),
        ("o / O", fl!("help-conn-sort")),
        ("p / Space", fl!("help-conn-pause")),
        ("y", fl!("help-conn-copy")),
        ("d / D", fl!("help-close-connections")),
        blank(),
        ("", fl!("help-logs")),
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
        blank(),
        ("", fl!("help-profiles")),
        ("Enter", fl!("help-profiles-menu")),
        ("e / E", fl!("help-profiles-edit")),
        ("n / i", fl!("help-profiles-new")),
        ("f / F", fl!("help-profiles-update")),
        ("d / A", fl!("help-profiles-delete")),
        blank(),
        ("", fl!("help-containers")),
        ("Enter / n", fl!("help-containers-menu")),
        ("t / o / !", fl!("help-containers-run")),
        ("e / E / i", fl!("help-containers-edit")),
        ("a / d", fl!("help-containers-delete")),
        ("C / U / A", fl!("help-containers-host")),
    ]);
    draw_help_columns(frame, &fl!("help-title"), (left, right), 110, None);
}

fn draw_confirm(frame: &mut Frame, message: &str) {
    let mut lines: Vec<Line> = vec![Line::raw("")];
    for (i, line) in message.lines().enumerate() {
        lines.push(if i == 0 {
            Line::from(Span::styled(line.to_owned(), Style::new().fg(TEXT).bold()))
        } else {
            Line::from(Span::styled(line.to_owned(), Style::new().fg(SUBTEXT)))
        });
    }
    lines.push(Line::raw(""));
    lines.push(Line::from(vec![
        Span::styled(
            format!("  y  {}  ", fl!("key-confirm")),
            Style::new().fg(CRUST).bg(YELLOW).bold(),
        ),
        Span::raw("    "),
        Span::styled(
            format!("  n  {}  ", fl!("key-cancel")),
            Style::new().fg(TEXT).bg(SURFACE2).bold(),
        ),
    ]));
    let width: u16 = 72;
    let height = wrapped_rows(&lines, width) + 3;
    let text = Text::from(lines);
    let area = popup_area(frame, width, height);
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(text)
            .alignment(Alignment::Center)
            .wrap(Wrap { trim: false })
            .block(dialog(&format!("⚠ {}", fl!("tui-panel-confirm")), YELLOW)),
        area,
    );
}

fn draw_message(frame: &mut Frame, title: &str, body: &str, error: bool, copyable: bool) {
    let screen = frame.area();
    let width = (screen.width * 4 / 5).max(40);
    let lines = body.lines().count() as u16;
    let height = (lines + 4).min(screen.height * 4 / 5).max(5);
    let rect = popup_area(frame, width, height);
    frame.render_widget(Clear, rect);
    let (color, icon) = if error { (RED, "✕") } else { (GREEN, "✓") };
    let hint = if copyable {
        fl!("tui-hint-copy-close")
    } else {
        fl!("tui-hint-close")
    };
    let block = dialog(&format!("{icon} {title}"), color)
        .title_bottom(Line::from(dim(format!(" {hint} "))).right_aligned());
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    frame.render_widget(
        Paragraph::new(body.to_owned())
            .fg(TEXT)
            .wrap(Wrap { trim: false }),
        inner.inner(Margin::new(0, 1)),
    );
}

fn draw_input(frame: &mut Frame, input: &Input) {
    let (before, after) = input.split();
    let mut field = vec![Span::styled("❯ ", Style::new().fg(ACCENT).bold())];
    if input.value.is_empty() {
        field.push(Span::styled("▏", Style::new().fg(ACCENT)));
        field.push(dim(input.placeholder.clone()));
    } else {
        let mut after = after.chars();
        let under = after.next();
        field.push(Span::styled(before.to_owned(), Style::new().fg(TEXT)));
        field.push(match under {
            Some(c) => Span::styled(c.to_string(), Style::new().fg(CRUST).bg(ACCENT)),
            None => Span::styled("▏", Style::new().fg(ACCENT)),
        });
        field.push(Span::styled(
            after.as_str().to_owned(),
            Style::new().fg(TEXT),
        ));
    }
    let width: u16 = 84;
    let hint: Vec<Line> = if input.hint.is_empty() {
        Vec::new()
    } else {
        input
            .hint
            .lines()
            .map(|line| Line::from(Span::styled(line.to_owned(), Style::new().fg(SUBTEXT))))
            .collect()
    };
    let hint_rows = if hint.is_empty() {
        0
    } else {
        wrapped_rows(&hint, width) + 1
    };
    let error_rows = u16::from(input.error.is_some());
    let height = 1 + hint_rows + 3 + error_rows + 2;
    let area = popup_area(frame, width, height);
    frame.render_widget(Clear, area);
    let block = dialog(&input.title, ACCENT).title_bottom(dialog_keys(&[
        ("⏎", fl!("key-submit")),
        ("Esc", fl!("key-cancel")),
        ("^U", fl!("key-clear")),
    ]));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let [_, hint_area, field_area, error_area] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(hint_rows),
        Constraint::Length(3),
        Constraint::Length(error_rows),
    ])
    .areas(inner);
    if !hint.is_empty() {
        frame.render_widget(Paragraph::new(hint).wrap(Wrap { trim: false }), hint_area);
    }
    let border = if input.error.is_some() { RED } else { SURFACE2 };
    frame.render_widget(
        Paragraph::new(Line::from(field)).block(
            Block::bordered()
                .border_type(ratatui::widgets::BorderType::Rounded)
                .border_style(Style::new().fg(border))
                .padding(ratatui::widgets::Padding::horizontal(1)),
        ),
        field_area,
    );
    if let Some(err) = &input.error {
        frame.render_widget(
            Paragraph::new(Span::styled(format!("✕ {err}"), Style::new().fg(RED))),
            error_area,
        );
    }
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
        Line::from(vec![
            Span::styled(MARK, Style::new().fg(ACCENT)),
            Span::styled(name, Style::new().fg(ACCENT).bold()),
        ]),
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
    lines.push(Line::from(Span::styled(
        question,
        Style::new().fg(TEXT).bold(),
    )));
    let width: u16 = 76;
    let height = wrapped_rows(&lines, width) + 2;
    let area = popup_area(frame, width, height);
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: false }).block(
            dialog(&fl!("setup-title"), ACCENT).title_bottom(dialog_keys(&[
                ("y", fl!("key-yes")),
                ("n", fl!("key-no")),
                ("Esc", fl!("key-ask-later")),
            ])),
        ),
        area,
    );
}

fn draw_menu(frame: &mut Frame, menu: &Menu) {
    let label_width = menu
        .items
        .iter()
        .map(|item| text_width(&item.label))
        .max()
        .unwrap_or(0);
    let detail_width = menu
        .items
        .iter()
        .map(|item| text_width(&item.detail))
        .max()
        .unwrap_or(0);
    let keys = dialog_keys(&[
        ("↑↓", fl!("key-move")),
        ("⏎", fl!("key-select")),
        ("Esc", fl!("key-cancel")),
    ]);
    let screen = frame.area();
    let width = ((label_width + detail_width + 10) as u16)
        .max(text_width(&menu.title) as u16 + 8)
        .max(keys.width() as u16 + 4)
        .clamp(40, screen.width.saturating_sub(4).max(40));
    let body: Vec<Line> = menu
        .body
        .as_deref()
        .map(|text| {
            text.lines()
                .take(12)
                .map(|line| Line::from(Span::styled(line.to_owned(), Style::new().fg(SUBTEXT))))
                .collect()
        })
        .unwrap_or_default();
    let body_rows = if body.is_empty() {
        0
    } else {
        wrapped_rows(&body, width) + 1
    };
    let height = (menu.items.len() as u16 + body_rows + 2).min(screen.height.saturating_sub(2));
    let area = popup_area(frame, width, height);
    frame.render_widget(Clear, area);
    let block = dialog(&menu.title, ACCENT).title_bottom(keys);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let [body_area, list_area] =
        Layout::vertical([Constraint::Length(body_rows), Constraint::Min(1)]).areas(inner);
    if !body.is_empty() {
        frame.render_widget(Paragraph::new(body).wrap(Wrap { trim: false }), body_area);
    }
    let items: Vec<ListItem> = menu
        .items
        .iter()
        .map(|item| {
            let pad = label_width.saturating_sub(text_width(&item.label));
            ListItem::new(Line::from(vec![
                Span::styled(
                    format!(" {}{}  ", item.label, " ".repeat(pad)),
                    Style::new().fg(TEXT),
                ),
                dim(item.detail.clone()),
            ]))
        })
        .collect();
    let mut state = ratatui::widgets::ListState::default().with_selected(Some(menu.selected));
    frame.render_stateful_widget(
        List::new(items)
            .highlight_style(selected(true))
            .highlight_symbol(Span::styled(MARK, Style::new().fg(ACCENT))),
        list_area,
        &mut state,
    );
    scrollbar(
        frame,
        area,
        (
            menu.items.len(),
            state.offset(),
            usize::from(list_area.height),
        ),
        true,
    );
}

fn draw_editor_help(frame: &mut Frame) {
    let blank = || ("", String::new());
    let left = help_lines(&[
        ("", fl!("help-editor-move")),
        ("↑↓ PgUp/PgDn", fl!("help-editor-cursor")),
        ("← →", fl!("help-editor-fold")),
        ("Space * -", fl!("help-editor-toggle")),
        ("/ n N", fl!("help-editor-search")),
        blank(),
        ("", fl!("help-editor-file")),
        ("s", fl!("help-editor-save")),
        ("u / U", fl!("help-editor-undo")),
        ("q / Esc / F2", fl!("help-editor-close")),
    ]);
    let right = help_lines(&[
        ("", fl!("help-editor-change")),
        ("Enter / e", fl!("help-editor-edit")),
        (": / E", fl!("help-editor-json")),
        ("a / A", fl!("help-editor-add")),
        ("r", fl!("help-editor-rename")),
        ("d", fl!("help-editor-delete")),
        ("c", fl!("help-editor-duplicate")),
        ("K / J", fl!("help-editor-reorder")),
        ("y", fl!("help-editor-copy")),
    ]);
    draw_help_columns(frame, &fl!("help-editor-title"), (left, right), 104, None);
}

fn draw_code_help(frame: &mut Frame) {
    let [left, right] = super::code::help_rows();
    let left = help_lines_with(&left, 13);
    let right = help_lines_with(&right, 13);
    draw_help_columns(
        frame,
        &fl!("help-code-title"),
        (left, right),
        116,
        Some(fl!("help-code-note")),
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

    #[test]
    fn colon_levels() {
        let line = "2026/10/7 02:50:36 [sub-store] WARN: UUID may be invalid";
        let (start, end, color) = find_colon_level(line).unwrap();
        assert_eq!((&line[start..end], color), ("WARN:", YELLOW));
        assert_eq!(find_colon_level("ERROR: boom").unwrap(), (0, 6, RED));
        assert!(find_colon_level("Timeout: 8000").is_none());
    }

    #[test]
    fn chart_scale_is_round() {
        assert_eq!(nice_ceiling(1024), 1024);
        assert_eq!(nice_ceiling(1500), 2048);
        assert_eq!(nice_ceiling(3_000_000), 5 * 1024 * 1024);
        assert_eq!(nice_ceiling(900), 1000);
    }
}
