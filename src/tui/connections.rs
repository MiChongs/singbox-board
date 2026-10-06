//! The "Connections" tab: the live connections of the Clash API with their
//! transfer rates, a filter, sort orders and the details of the selected
//! connection.

use std::cmp::{Ordering, Reverse};
use std::collections::HashMap;
use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, BorderType, Cell, Padding, Paragraph, Row, Sparkline, Table};
use ratatui::widgets::{TableState, Wrap};

use super::app::{App, PendingAction, Popup, move_table};
use super::popup::{Input, InputPurpose};
use super::theme::{
    ACCENT, BLUE, BORDER, DIM, DOWN, GREEN, MARK, PEACH, RED, SKY, SPINNER, SUBTEXT, SURFACE2,
    TEXT, UP, YELLOW, chip, dim, header_row, label, panel, pill,
};
use crate::clash::Connection;
use crate::i18n::fl;
use crate::protocol::CoreState;
use crate::util::{fmt_bytes, fmt_duration, fmt_speed, text_width, truncate, truncate_start};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SortKey {
    #[default]
    Newest,
    Rate,
    Traffic,
    Host,
}

impl SortKey {
    fn next(self) -> Self {
        match self {
            SortKey::Newest => SortKey::Rate,
            SortKey::Rate => SortKey::Traffic,
            SortKey::Traffic => SortKey::Host,
            SortKey::Host => SortKey::Newest,
        }
    }

    fn label(self) -> String {
        match self {
            SortKey::Newest => fl!("conn-sort-newest"),
            SortKey::Rate => fl!("conn-sort-rate"),
            SortKey::Traffic => fl!("conn-sort-traffic"),
            SortKey::Host => fl!("conn-sort-host"),
        }
    }

    /// Hosts sort A to Z, everything else largest or newest first.
    fn descending(self) -> bool {
        self != SortKey::Host
    }
}

/// Upload and download rate in bytes per second.
type Rates = HashMap<String, (u64, u64)>;

#[derive(Default)]
pub struct ConnectionsView {
    /// The connections on screen.
    pub list: Vec<Connection>,
    rates: Rates,
    /// Indices into `list` that pass the filter, in display order.
    pub rows: Vec<usize>,
    pub state: TableState,
    pub filter: String,
    pub sort: SortKey,
    pub reverse: bool,
    /// Holds the list still so that rows stop moving while reading.
    pub paused: bool,
    pub hide_details: bool,
    /// Whether a snapshot arrived since the Clash API became reachable.
    pub loaded: bool,
    /// The newest snapshot while paused, shown on resume.
    held: Option<(Vec<Connection>, Rates)>,
    /// Bytes each connection had moved at the last snapshot.
    seen: HashMap<String, (u64, u64)>,
    seen_at: Option<Instant>,
}

impl ConnectionsView {
    /// Takes in a snapshot of `/connections` and works out the rates.
    pub fn take(&mut self, list: Vec<Connection>) {
        let now = Instant::now();
        let secs = self
            .seen_at
            .map(|at| now.duration_since(at).as_secs_f64().max(0.001));
        let rates = list
            .iter()
            .map(|c| {
                let rate = secs.map_or((0, 0), |secs| {
                    // A connection opened since the last snapshot moved all
                    // of its bytes since then.
                    let (up, down) = self.seen.get(&c.id).copied().unwrap_or_default();
                    (
                        (c.upload.saturating_sub(up) as f64 / secs) as u64,
                        (c.download.saturating_sub(down) as f64 / secs) as u64,
                    )
                });
                (c.id.clone(), rate)
            })
            .collect();
        self.seen = list
            .iter()
            .map(|c| (c.id.clone(), (c.upload, c.download)))
            .collect();
        self.seen_at = Some(now);
        self.loaded = true;
        if self.paused {
            self.held = Some((list, rates));
        } else {
            self.show(list, rates);
        }
    }

    /// Forgets everything once the Clash API is unreachable.
    pub fn clear(&mut self) {
        self.list.clear();
        self.rates.clear();
        self.rows.clear();
        self.held = None;
        self.seen.clear();
        self.seen_at = None;
        self.loaded = false;
        self.state.select(None);
    }

    fn show(&mut self, list: Vec<Connection>, rates: Rates) {
        let keep = self.selected().map(|c| c.id.clone());
        self.list = list;
        self.rates = rates;
        self.arrange(keep);
    }

    /// Rebuilds the rows after a change of the list, filter or order, and
    /// keeps the cursor on the same connection while it is still shown.
    fn arrange(&mut self, keep: Option<String>) {
        let terms: Vec<String> = self
            .filter
            .split_whitespace()
            .map(str::to_lowercase)
            .collect();
        let list = &self.list;
        let mut rows: Vec<usize> = (0..list.len())
            .filter(|&i| terms.is_empty() || matches(&list[i], &terms))
            .collect();
        let rate = |i: usize| {
            let (up, down) = self.rates.get(&list[i].id).copied().unwrap_or_default();
            up + down
        };
        let newest = |a: usize, b: usize| {
            list[b]
                .start
                .cmp(&list[a].start)
                .then_with(|| list[a].id.cmp(&list[b].id))
        };
        let order = |a: usize, b: usize| -> Ordering {
            let by_key = match self.sort {
                SortKey::Newest => Ordering::Equal,
                SortKey::Rate => Reverse(rate(a)).cmp(&Reverse(rate(b))),
                SortKey::Traffic => Reverse(list[a].upload + list[a].download)
                    .cmp(&Reverse(list[b].upload + list[b].download)),
                SortKey::Host => list[a].host().cmp(list[b].host()),
            };
            by_key.then_with(|| newest(a, b))
        };
        rows.sort_by(|&a, &b| order(a, b));
        if self.reverse {
            rows.reverse();
        }
        self.rows = rows;
        let index = keep
            .and_then(|id| self.rows.iter().position(|&i| self.list[i].id == id))
            .or(self.state.selected())
            .map(|i| i.min(self.rows.len().saturating_sub(1)));
        self.state.select(if self.rows.is_empty() {
            None
        } else {
            index.or(Some(0))
        });
    }

    pub fn selected(&self) -> Option<&Connection> {
        self.state
            .selected()
            .and_then(|i| self.rows.get(i))
            .and_then(|&i| self.list.get(i))
    }

    /// Connections right now, even while the list is paused.
    pub fn live_len(&self) -> usize {
        self.held
            .as_ref()
            .map_or(self.list.len(), |(list, _)| list.len())
    }

    pub fn rate(&self, id: &str) -> (u64, u64) {
        self.rates.get(id).copied().unwrap_or_default()
    }

    fn is_active(&self, id: &str) -> bool {
        self.rate(id) != (0, 0)
    }

    pub fn set_filter(&mut self, text: &str) {
        let keep = self.selected().map(|c| c.id.clone());
        self.filter = text.trim().to_owned();
        self.arrange(keep);
    }

    fn cycle_sort(&mut self) {
        let keep = self.selected().map(|c| c.id.clone());
        self.sort = self.sort.next();
        self.reverse = false;
        self.arrange(keep);
    }

    fn toggle_reverse(&mut self) {
        let keep = self.selected().map(|c| c.id.clone());
        self.reverse = !self.reverse;
        self.arrange(keep);
    }

    fn toggle_pause(&mut self) {
        self.paused = !self.paused;
        if !self.paused
            && let Some((list, rates)) = self.held.take()
        {
            self.show(list, rates);
        }
    }

    /// Connections per outbound, most used first.
    fn outbounds(&self) -> Vec<(&str, usize)> {
        let mut counts: HashMap<&str, usize> = HashMap::new();
        for c in &self.list {
            *counts.entry(c.outbound().unwrap_or("-")).or_default() += 1;
        }
        let mut counts: Vec<(&str, usize)> = counts.into_iter().collect();
        counts.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
        counts
    }

    /// The connection moving the most bytes right now.
    fn busiest(&self) -> Option<&Connection> {
        self.list
            .iter()
            .filter(|c| self.is_active(&c.id))
            .max_by_key(|c| {
                let (up, down) = self.rate(&c.id);
                up + down
            })
    }
}

/// Whether every filter term appears in one of the connection's fields.
fn matches(c: &Connection, terms: &[String]) -> bool {
    let m = &c.metadata;
    let mut text = [
        c.target().as_str(),
        &m.host,
        &m.sniff_host,
        &m.destination_ip,
        &m.source_ip,
        &m.network,
        &m.inbound,
        &m.process_path,
        &c.rule,
        &c.rule_payload,
    ]
    .join("\n");
    for link in &c.chains {
        text.push('\n');
        text.push_str(link);
    }
    let text = text.to_lowercase();
    terms.iter().all(|term| text.contains(term.as_str()))
}

impl App {
    pub(super) fn connections_on_key(&mut self, key: KeyEvent) {
        let view = &mut self.connections;
        let len = view.rows.len();
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => move_table(&mut view.state, len, -1),
            KeyCode::Down | KeyCode::Char('j') => move_table(&mut view.state, len, 1),
            KeyCode::PageUp => move_table(&mut view.state, len, -20),
            KeyCode::PageDown => move_table(&mut view.state, len, 20),
            KeyCode::Home | KeyCode::Char('g') => move_table(&mut view.state, len, isize::MIN / 2),
            KeyCode::End | KeyCode::Char('G') => move_table(&mut view.state, len, isize::MAX / 2),
            KeyCode::Enter | KeyCode::Char('i') => view.hide_details = !view.hide_details,
            KeyCode::Esc if !view.filter.is_empty() => view.set_filter(""),
            KeyCode::Char('o') => view.cycle_sort(),
            KeyCode::Char('O') => view.toggle_reverse(),
            KeyCode::Char('p' | ' ') => view.toggle_pause(),
            KeyCode::Char('/') => self.connections_filter_input(),
            KeyCode::Char('y') => {
                if let Some(target) = self.connections.selected().map(Connection::target) {
                    self.copy(target.clone(), fl!("tui-copied-target", target = target));
                }
            }
            KeyCode::Char('d') | KeyCode::Delete => self.close_selected_connection(),
            KeyCode::Char('D') => self.confirm_close_connections(),
            _ => {}
        }
    }

    fn connections_filter_input(&mut self) {
        let current = self.connections.filter.clone();
        self.popup = Some(Popup::Input(
            Input::new(
                fl!("conn-filter-title"),
                fl!("conn-filter-hint"),
                InputPurpose::ConnectionFilter(current.clone()),
            )
            .placeholder(fl!("conn-filter-placeholder"))
            .value(current),
        ));
    }

    /// Applies the filter while it is typed.
    pub(super) fn connections_filter_typed(&mut self, input: &Input) {
        if matches!(input.purpose, InputPurpose::ConnectionFilter(_)) {
            self.connections.set_filter(&input.value);
        }
    }

    fn close_selected_connection(&mut self) {
        let Some(connection) = self.connections.selected() else {
            return;
        };
        let id = connection.id.clone();
        let target = connection.target();
        self.clash_action(&fl!("busy-closing-connection"), move |clash| async move {
            clash.close_connection(&id).await?;
            Ok(fl!("tui-connection-closed", target = target))
        });
    }

    /// Closes every connection, or the ones the filter shows.
    fn confirm_close_connections(&mut self) {
        let view = &self.connections;
        let (message, action) = if view.filter.is_empty() {
            if view.live_len() == 0 {
                return;
            }
            (
                fl!("tui-confirm-close-all", count = view.live_len()),
                PendingAction::CloseAllConnections,
            )
        } else {
            let ids: Vec<String> = view.rows.iter().map(|&i| view.list[i].id.clone()).collect();
            if ids.is_empty() {
                return;
            }
            (
                fl!(
                    "tui-confirm-close-filtered",
                    count = ids.len(),
                    filter = view.filter.clone()
                ),
                PendingAction::CloseConnections { ids },
            )
        };
        self.popup = Some(Popup::Confirm {
            message: format!("{message}\n{}", fl!("tui-close-connections-note")),
            action,
        });
    }
}

// ----- rendering -------------------------------------------------------------

/// Rows the table keeps before the details and the stats get room: the
/// border, the header and eight connections.
const TABLE_ROWS: u16 = 11;
const STATS_ROWS: u16 = 4;

pub fn draw(frame: &mut Frame, area: Rect, app: &mut App) {
    let details = app
        .connections
        .selected()
        .filter(|_| !app.connections.hide_details)
        .map(|c| detail_lines(app, c, area.width))
        .filter(|(left, right)| area.height >= TABLE_ROWS + left.len().max(right.len()) as u16 + 2);
    let details_height = details
        .as_ref()
        .map_or(0, |(left, right)| left.len().max(right.len()) as u16 + 2);
    let stats_height = if area.height >= TABLE_ROWS + details_height + STATS_ROWS {
        STATS_ROWS
    } else {
        0
    };
    let [stats_area, table_area, details_area] = Layout::vertical([
        Constraint::Length(stats_height),
        Constraint::Min(0),
        Constraint::Length(details_height),
    ])
    .areas(area);
    if stats_height > 0 {
        draw_stats(frame, stats_area, app);
    }
    draw_table(frame, table_area, app);
    if let Some((left, right)) = details {
        draw_details(frame, details_area, left, right);
    }
}

/// A small titled box of the stats row.
fn card(title: String, color: Color) -> Block<'static> {
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(BORDER))
        .title(Line::from(Span::styled(
            format!(" {title} "),
            Style::new().fg(color).add_modifier(Modifier::BOLD),
        )))
        .padding(Padding::horizontal(1))
}

fn draw_stats(frame: &mut Frame, area: Rect, app: &App) {
    let mut constraints = vec![
        Constraint::Fill(3),
        Constraint::Fill(3),
        Constraint::Fill(3),
    ];
    if area.width >= 96 {
        constraints.push(Constraint::Fill(4));
    }
    if area.width >= 132 {
        constraints.push(Constraint::Fill(4));
    }
    let areas = Layout::horizontal(constraints).split(area);
    draw_count_card(frame, areas[0], app);
    let t = &app.traffic;
    draw_rate_card(
        frame,
        areas[1],
        (fl!("conn-card-download"), "↓", DOWN),
        (t.down_speed, t.down_total),
        &t.down_history,
    );
    draw_rate_card(
        frame,
        areas[2],
        (fl!("conn-card-upload"), "↑", UP),
        (t.up_speed, t.up_total),
        &t.up_history,
    );
    if let Some(&area) = areas.get(3) {
        draw_outbounds_card(frame, area, app);
    }
    if let Some(&area) = areas.get(4) {
        draw_busiest_card(frame, area, app);
    }
}

fn draw_count_card(frame: &mut Frame, area: Rect, app: &App) {
    let view = &app.connections;
    let title = fl!("conn-card-connections");
    let memory = format!(
        " {} ",
        fl!("conn-memory", bytes = fmt_bytes(app.traffic.memory))
    );
    let mut block = card(title.clone(), ACCENT);
    // Only beside the title, never over it.
    if text_width(&title) + text_width(&memory) + 6 <= usize::from(area.width) {
        block = block.title_top(Line::from(dim(memory)).right_aligned());
    }
    let lines = if view.loaded {
        let count = |network: &str| {
            view.list
                .iter()
                .filter(|c| c.metadata.network.eq_ignore_ascii_case(network))
                .count()
        };
        let active = view.list.iter().filter(|c| view.is_active(&c.id)).count();
        vec![
            Line::from(vec![
                Span::styled(
                    view.list.len().to_string(),
                    Style::new().fg(TEXT).add_modifier(Modifier::BOLD),
                ),
                Span::raw("  "),
                Span::styled(format!("TCP {}", count("tcp")), Style::new().fg(BLUE)),
                dim(" · "),
                Span::styled(format!("UDP {}", count("udp")), Style::new().fg(PEACH)),
            ]),
            if active > 0 {
                Line::from(vec![
                    Span::styled("● ", Style::new().fg(GREEN)),
                    dim(fl!("conn-active", count = active)),
                ])
            } else {
                Line::from(dim(fl!("conn-idle")))
            },
        ]
    } else {
        vec![Line::from(dim("-"))]
    };
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

fn draw_rate_card(
    frame: &mut Frame,
    area: Rect,
    (title, arrow, color): (String, &str, Color),
    (speed, total): (u64, u64),
    history: &std::collections::VecDeque<u64>,
) {
    let block = card(title, color);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let [value_area, spark_area] =
        Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).areas(inner);
    let speed = format!("{arrow} {}", fmt_speed(speed));
    let total = format!("  {}", fl!("tui-traffic-total", bytes = fmt_bytes(total)));
    let mut spans = vec![Span::styled(
        speed.clone(),
        Style::new().fg(color).add_modifier(Modifier::BOLD),
    )];
    if text_width(&speed) + text_width(&total) <= inner.width as usize {
        spans.push(dim(total));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), value_area);
    let data: Vec<u64> = history
        .iter()
        .skip(history.len().saturating_sub(spark_area.width as usize))
        .copied()
        .collect();
    let max = data.iter().copied().max().unwrap_or(0).max(1);
    frame.render_widget(
        Sparkline::default()
            .data(&data)
            .max(max)
            .style(Style::new().fg(color)),
        spark_area,
    );
}

fn draw_outbounds_card(frame: &mut Frame, area: Rect, app: &App) {
    let block = card(fl!("conn-card-outbounds"), SKY);
    let width = block.inner(area).width as usize;
    let mut lines: Vec<Vec<Span>> = vec![Vec::new()];
    let mut used = 0;
    for (name, count) in app.connections.outbounds() {
        let count = format!(" {count}");
        let entry = text_width(name) + text_width(&count);
        let gap = if used == 0 { 0 } else { 2 };
        if used > 0 && used + gap + entry > width {
            if lines.len() == 2 {
                break;
            }
            lines.push(Vec::new());
            used = 0;
        }
        let line = lines.last_mut().expect("a line");
        if used > 0 {
            line.push(Span::raw("  "));
            used += 2;
        }
        line.push(Span::styled(
            name.to_owned(),
            Style::new().fg(outbound_color(app, name)),
        ));
        line.push(dim(count));
        used += entry;
    }
    let lines: Vec<Line> = if lines[0].is_empty() {
        vec![Line::from(dim("-"))]
    } else {
        lines.into_iter().map(Line::from).collect()
    };
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

fn draw_busiest_card(frame: &mut Frame, area: Rect, app: &App) {
    let block = card(fl!("conn-card-busiest"), GREEN);
    let lines = match app.connections.busiest() {
        Some(c) => {
            let (up, down) = app.connections.rate(&c.id);
            vec![
                Line::from(target_spans(c, true, Some(card_width(area)))),
                Line::from(vec![
                    Span::styled(format!("↓ {}", fmt_speed(down)), Style::new().fg(DOWN)),
                    Span::raw("  "),
                    Span::styled(format!("↑ {}", fmt_speed(up)), Style::new().fg(UP)),
                ]),
            ]
        }
        None => vec![Line::from(dim(fl!("conn-idle")))],
    };
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

/// Columns inside a card.
fn card_width(area: Rect) -> usize {
    usize::from(area.width.saturating_sub(4))
}

/// `host` bright when it is a domain, then a dim `:port`. A long host
/// loses its start rather than its domain: `…logs.example.com:443`.
fn target_spans(c: &Connection, bold: bool, width: Option<usize>) -> Vec<Span<'static>> {
    let mut style = Style::new().fg(if c.has_domain() { TEXT } else { SUBTEXT });
    if bold {
        style = style.add_modifier(Modifier::BOLD);
    }
    let host = c.host();
    let host = if host.contains(':') {
        format!("[{host}]")
    } else {
        host.to_owned()
    };
    let port = format!(":{}", c.metadata.destination_port);
    match width {
        Some(width) if text_width(&host) + text_width(&port) > width => {
            let room = width.saturating_sub(text_width(&port));
            if room >= 8 {
                vec![Span::styled(truncate_start(&host, room), style), dim(port)]
            } else {
                vec![Span::styled(truncate_start(&host, width), style)]
            }
        }
        _ => vec![Span::styled(host, style), dim(port)],
    }
}

/// Direct traffic is green, blocked traffic red, proxied traffic violet.
fn outbound_color(app: &App, name: &str) -> Color {
    let kind = app.group(name).map_or(name, |p| p.kind.as_str());
    if kind.eq_ignore_ascii_case("direct") {
        GREEN
    } else if ["block", "reject"]
        .iter()
        .any(|k| kind.eq_ignore_ascii_case(k))
    {
        RED
    } else {
        ACCENT
    }
}

/// The rule's group first and the node last, shortened to `width`.
fn chain_line(app: &App, c: &Connection, width: Option<usize>) -> Line<'static> {
    // sing-box lists the final node first.
    let links: Vec<&str> = c.chains.iter().rev().map(String::as_str).collect();
    let Some((&node, groups)) = links.split_last() else {
        return Line::from(dim("-"));
    };
    let (groups, node_text) = match width {
        Some(width) => fit_chain(groups, node, width),
        None => (groups.to_vec(), node.to_owned()),
    };
    let mut spans = Vec::new();
    for group in groups {
        let color = if group == "…" { DIM } else { SUBTEXT };
        spans.push(Span::styled(group.to_owned(), Style::new().fg(color)));
        spans.push(dim(" → "));
    }
    spans.push(Span::styled(
        node_text,
        Style::new()
            .fg(outbound_color(app, node))
            .add_modifier(Modifier::BOLD),
    ));
    Line::from(spans)
}

/// The groups of a chain, and its node, that fit `width`: the groups in
/// between fold into `…`, then go, then the rule's group goes too, so that
/// the node stays visible.
fn fit_chain<'a>(groups: &[&'a str], node: &str, width: usize) -> (Vec<&'a str>, String) {
    let fits = |groups: &[&str]| {
        groups.iter().map(|g| text_width(g) + 3).sum::<usize>() + text_width(node) <= width
    };
    let mut shorter = vec![groups.to_vec()];
    if let [first, _, ..] = groups {
        shorter.push(vec![*first, "…"]);
        shorter.push(vec![*first]);
    }
    shorter.push(Vec::new());
    match shorter.into_iter().find(|groups| fits(groups)) {
        Some(groups) => (groups, node.to_owned()),
        None => (Vec::new(), truncate(node, width)),
    }
}

fn network_span(network: &str) -> Span<'static> {
    let color = match network.to_ascii_lowercase().as_str() {
        "tcp" => BLUE,
        "udp" => PEACH,
        _ => SUBTEXT,
    };
    Span::styled(network.to_uppercase(), Style::new().fg(color))
}

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

fn started_at(start: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(start)
        .map(|t| {
            t.with_timezone(&chrono::Local)
                .format("%Y-%m-%d %H:%M:%S")
                .to_string()
        })
        .unwrap_or_else(|_| start.to_owned())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Column {
    Activity,
    Target,
    Network,
    Process,
    Chain,
    Rule,
    DownRate,
    UpRate,
    Download,
    Upload,
    Age,
}

impl Column {
    /// Left to right.
    const ALL: [Column; 11] = [
        Column::Activity,
        Column::Target,
        Column::Network,
        Column::Process,
        Column::Chain,
        Column::Rule,
        Column::DownRate,
        Column::UpRate,
        Column::Download,
        Column::Upload,
        Column::Age,
    ];

    /// The last ones go first when the table is narrow.
    const PRIORITY: [Column; 11] = [
        Column::Activity,
        Column::Target,
        Column::DownRate,
        Column::Chain,
        Column::Network,
        Column::UpRate,
        Column::Age,
        Column::Download,
        Column::Rule,
        Column::Upload,
        Column::Process,
    ];

    /// Layout constraint and the width below which the column is dropped.
    fn width(self, age: u16) -> (Constraint, u16) {
        match self {
            Column::Activity => (Constraint::Length(1), 1),
            Column::Target => (Constraint::Fill(4), 20),
            Column::Network => (Constraint::Length(4), 4),
            Column::Process => (Constraint::Fill(2), 8),
            Column::Chain => (Constraint::Fill(4), 16),
            Column::Rule => (Constraint::Fill(2), 10),
            Column::DownRate | Column::UpRate => (Constraint::Length(12), 12),
            Column::Download | Column::Upload => (Constraint::Length(10), 10),
            Column::Age => (Constraint::Length(age), age),
        }
    }

    fn numeric(self) -> bool {
        matches!(
            self,
            Column::DownRate | Column::UpRate | Column::Download | Column::Upload | Column::Age
        )
    }

    fn header(self) -> Line<'static> {
        let arrow = |arrow: &str, color: Color, text: String| {
            Line::from(vec![
                Span::styled(format!("{arrow} "), Style::new().fg(color)),
                Span::raw(text),
            ])
        };
        let line = match self {
            Column::Activity => Line::raw(""),
            Column::Target => Line::raw(fl!("col-destination")),
            Column::Network => Line::raw(fl!("col-network")),
            Column::Process => Line::raw(fl!("col-process")),
            Column::Chain => Line::raw(fl!("col-chain")),
            Column::Rule => Line::raw(fl!("col-rule")),
            Column::DownRate => arrow("↓", DOWN, fl!("col-rate")),
            Column::UpRate => arrow("↑", UP, fl!("col-rate")),
            Column::Download => arrow("↓", DOWN, fl!("col-traffic")),
            Column::Upload => arrow("↑", UP, fl!("col-traffic")),
            Column::Age => Line::raw(fl!("col-age")),
        };
        if self.numeric() {
            line.right_aligned()
        } else {
            line
        }
    }
}

/// The columns that fit in `width`, in display order.
fn columns(width: u16, age: u16, process: bool) -> Vec<Column> {
    let mut chosen: Vec<Column> = Column::PRIORITY
        .into_iter()
        .filter(|c| process || *c != Column::Process)
        .collect();
    // Each column takes a space before it: the highlight mark, then the gaps.
    let need = |cols: &[Column]| cols.iter().map(|c| c.width(age).1 + 1).sum::<u16>();
    while chosen.len() > 2 && need(&chosen) > width {
        chosen.pop();
    }
    Column::ALL
        .into_iter()
        .filter(|c| chosen.contains(c))
        .collect()
}

fn cell(app: &App, c: &Connection, column: Column, width: usize) -> Cell<'static> {
    let (up, down) = app.connections.rate(&c.id);
    let rate = |rate: u64, color: Color| {
        if rate == 0 {
            dim("-")
        } else {
            Span::styled(fmt_speed(rate), Style::new().fg(color))
        }
    };
    let line = match column {
        Column::Activity => Line::from(if up + down > 0 {
            Span::styled("●", Style::new().fg(GREEN))
        } else {
            Span::styled("○", Style::new().fg(DIM))
        }),
        Column::Target => Line::from(target_spans(c, false, Some(width))),
        Column::Network => Line::from(network_span(&c.metadata.network)),
        Column::Process => Line::from(match c.process_name() {
            Some(name) => Span::styled(truncate(name, width), Style::new().fg(SUBTEXT)),
            None => dim("-"),
        }),
        Column::Chain => chain_line(app, c, Some(width)),
        Column::Rule => Line::from(dim(truncate(&c.rule_label(), width))),
        Column::DownRate => Line::from(rate(down, DOWN)),
        Column::UpRate => Line::from(rate(up, UP)),
        Column::Download => Line::from(Span::styled(
            fmt_bytes(c.download),
            Style::new().fg(SUBTEXT),
        )),
        Column::Upload => Line::from(Span::styled(fmt_bytes(c.upload), Style::new().fg(SUBTEXT))),
        Column::Age => Line::from(dim(age(&c.start))),
    };
    Cell::from(if column.numeric() {
        line.right_aligned()
    } else {
        line
    })
}

fn draw_table(frame: &mut Frame, area: Rect, app: &mut App) {
    let view = &app.connections;
    let title = if view.filter.is_empty() {
        fl!("tui-panel-connections", count = view.list.len())
    } else {
        fl!(
            "tui-panel-connections-filtered",
            shown = view.rows.len(),
            count = view.list.len()
        )
    };
    let mut status = vec![Span::raw(" ")];
    if view.paused {
        status.push(pill(fl!("conn-paused"), YELLOW));
        status.push(Span::raw(" "));
    }
    if !view.filter.is_empty() {
        status.push(chip(
            fl!("conn-filter-label", filter = view.filter.clone()),
            YELLOW,
        ));
        status.push(Span::raw(" "));
    }
    let arrow = if view.sort.descending() != view.reverse {
        "▼"
    } else {
        "▲"
    };
    status.push(dim(format!(
        "{} {arrow} ",
        fl!("conn-sort-label", key = view.sort.label())
    )));
    let mut block = panel(&title, true).title_top(Line::from(status).right_aligned());
    if let Some(index) = view.state.selected().filter(|_| !view.rows.is_empty()) {
        block = block.title_bottom(
            Line::from(dim(format!(" {} / {} ", index + 1, view.rows.len()))).right_aligned(),
        );
    }

    if let Some((icon, color, title, hint)) = empty_state(app) {
        let inner = block.inner(area);
        frame.render_widget(block, area);
        let mut lines = vec![Line::from(vec![
            Span::styled(format!("{icon}  "), Style::new().fg(color)),
            Span::styled(title, Style::new().fg(TEXT).add_modifier(Modifier::BOLD)),
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
            Constraint::Length(inner.height.saturating_sub(rows) / 2),
            Constraint::Min(0),
        ])
        .areas(inner);
        frame.render_widget(
            Paragraph::new(text)
                .alignment(Alignment::Center)
                .wrap(Wrap { trim: true }),
            message,
        );
        return;
    }

    let ages: Vec<u16> = view
        .rows
        .iter()
        .map(|&i| text_width(&age(&view.list[i].start)) as u16)
        .collect();
    let age_width = ages
        .into_iter()
        .chain([text_width(&fl!("col-age")) as u16, 5])
        .max()
        .unwrap_or(5);
    let process = view.list.iter().any(|c| c.process().is_some());
    let inner = block.inner(area);
    let columns = columns(inner.width, age_width, process);
    // Laid out here as the table would, so that cells can fit their text.
    let widths: Vec<u16> = Layout::horizontal(columns.iter().map(|c| c.width(age_width).0))
        .spacing(1)
        .split(Rect {
            width: inner.width.saturating_sub(1),
            ..inner
        })
        .iter()
        .map(|rect| rect.width)
        .collect();
    let rows: Vec<Row> = view
        .rows
        .iter()
        .map(|&i| {
            let c = &view.list[i];
            Row::new(
                columns
                    .iter()
                    .zip(&widths)
                    .map(|(&column, &width)| cell(app, c, column, usize::from(width))),
            )
        })
        .collect();
    let table = Table::new(rows, widths.iter().map(|&w| Constraint::Length(w)))
        .header(Row::new(columns.iter().map(|c| Cell::from(c.header()))).style(header_row()))
        .block(block)
        // A background alone keeps the colours of the selected row.
        .row_highlight_style(Style::new().bg(SURFACE2).add_modifier(Modifier::BOLD))
        .highlight_symbol(Span::styled(MARK, Style::new().fg(ACCENT)));
    frame.render_stateful_widget(table, area, &mut app.connections.state);
}

/// Icon, colour, title and hint of a table without rows.
fn empty_state(app: &App) -> Option<(&'static str, Color, String, String)> {
    let view = &app.connections;
    let Some(status) = &app.status else {
        return Some((
            SPINNER[app.frame % SPINNER.len()],
            YELLOW,
            fl!("tui-waiting-daemon"),
            app.status_error.clone().unwrap_or_default(),
        ));
    };
    if status.clash_api.is_none() {
        return Some(("⚠", YELLOW, fl!("conn-no-api"), fl!("conn-no-api-hint")));
    }
    if !view.loaded {
        return Some(match status.state {
            CoreState::Stopped | CoreState::Failed => (
                "○",
                DIM,
                fl!("conn-not-running"),
                fl!("conn-not-running-hint"),
            ),
            _ => match &app.clash_error {
                Some(err) if status.state == CoreState::Running => (
                    "✕",
                    RED,
                    fl!("conn-api-error"),
                    format!(
                        "{}\n{}",
                        err.lines().next().unwrap_or_default(),
                        fl!("conn-api-error-hint")
                    ),
                ),
                _ => (
                    SPINNER[app.frame % SPINNER.len()],
                    YELLOW,
                    fl!("tui-loading"),
                    String::new(),
                ),
            },
        });
    }
    if view.list.is_empty() {
        return Some(("○", DIM, fl!("conn-empty"), fl!("conn-empty-hint")));
    }
    if view.rows.is_empty() {
        return Some((
            "⌕",
            YELLOW,
            fl!("conn-no-match", filter = view.filter.clone()),
            fl!("conn-no-match-hint"),
        ));
    }
    None
}

type DetailLines = (Vec<Line<'static>>, Vec<Line<'static>>);

/// Two columns of facts about `c`, or one when the screen is narrow.
fn detail_lines(app: &App, c: &Connection, width: u16) -> DetailLines {
    let labels = [
        fl!("conn-field-target"),
        fl!("conn-field-address"),
        fl!("conn-field-source"),
        fl!("conn-field-process"),
        fl!("conn-field-chain"),
        fl!("conn-field-rule"),
        fl!("conn-field-started"),
        fl!("conn-field-traffic"),
    ];
    let field = labels.iter().map(|l| text_width(l)).max().unwrap_or(0) + 2;
    let row = |index: usize, spans: Vec<Span<'static>>| {
        let mut line = vec![label(&labels[index], field)];
        line.extend(spans);
        Line::from(line)
    };
    let m = &c.metadata;
    let (up, down) = app.connections.rate(&c.id);

    let mut target = target_spans(c, true, None);
    target.push(Span::raw("  "));
    target.push(network_span(&m.network));
    if m.host.is_empty() && !m.sniff_host.is_empty() {
        target.push(Span::raw("  "));
        target.push(chip(fl!("conn-sniffed"), SKY));
    }
    // The address only adds something when the target is a domain.
    let address = c.has_domain().then(|| {
        vec![Span::styled(
            c.address().unwrap_or_else(|| "-".to_owned()),
            Style::new().fg(SUBTEXT),
        )]
    });
    let mut source = vec![Span::styled(
        c.source().unwrap_or_else(|| "-".to_owned()),
        Style::new().fg(SUBTEXT),
    )];
    if !m.inbound.is_empty() {
        source.push(dim(format!("  {}  ", fl!("conn-inbound"))));
        source.push(Span::styled(m.inbound.clone(), Style::new().fg(SKY)));
    }
    // Columns for the values: two columns side by side on wide screens.
    let two_columns = width >= 100;
    let inner = usize::from(width.saturating_sub(4));
    let room = if two_columns {
        inner.saturating_sub(2) / 2
    } else {
        inner
    }
    .saturating_sub(field);
    let process = match c.process() {
        Some((path, user)) => {
            let mut spans = vec![Span::styled(
                c.process_name().unwrap_or(path).to_owned(),
                Style::new().fg(TEXT).add_modifier(Modifier::BOLD),
            )];
            if let Some(user) = user {
                spans.push(Span::raw("  "));
                spans.push(chip(user.to_owned(), BLUE));
            }
            // The end of a long path says the most: …/versions/2.1.289.
            let left = room.saturating_sub(Line::from(spans.clone()).width() + 2);
            if c.process_name() != Some(path) && left >= 12 {
                spans.push(dim(format!("  {}", truncate_start(path, left))));
            }
            spans
        }
        None => vec![dim(fl!("unknown"))],
    };
    let mut chain = chain_line(app, c, None).spans;
    if let Some(kind) = c
        .outbound()
        .and_then(|node| app.group(node))
        .map(|p| p.kind.clone())
        .filter(|kind| !kind.is_empty())
    {
        chain.push(dim(format!("  {kind}")));
    }
    let mut rule = vec![Span::styled(c.rule_label(), Style::new().fg(TEXT))];
    if let Some(action) = c.rule_action() {
        rule.push(dim("  ⇒ "));
        rule.push(Span::styled(action.to_owned(), Style::new().fg(ACCENT)));
    }
    let started = vec![
        Span::styled(started_at(&c.start), Style::new().fg(SUBTEXT)),
        dim(format!("  {}", fl!("conn-lasted", age = age(&c.start)))),
    ];
    let traffic = |arrow: &str, bytes: u64, rate: u64, color: Color| {
        let mut spans = vec![Span::styled(
            format!("{arrow} {}", fmt_bytes(bytes)),
            Style::new().fg(color).add_modifier(Modifier::BOLD),
        )];
        if rate > 0 {
            spans.push(Span::styled(
                format!("  {}", fmt_speed(rate)),
                Style::new().fg(color),
            ));
        }
        spans
    };

    if two_columns {
        (
            [
                Some(row(0, target)),
                address.map(|address| row(1, address)),
                Some(row(2, source)),
                Some(row(3, process)),
            ]
            .into_iter()
            .flatten()
            .collect(),
            vec![
                row(4, chain),
                row(5, rule),
                row(6, started),
                Line::from({
                    let mut line = vec![label(&labels[7], field)];
                    line.extend(traffic("↓", c.download, down, DOWN));
                    line.push(Span::raw("   "));
                    line.extend(traffic("↑", c.upload, up, UP));
                    line
                }),
            ],
        )
    } else {
        let mut traffic_line = traffic("↓", c.download, down, DOWN);
        traffic_line.push(Span::raw("   "));
        traffic_line.extend(traffic("↑", c.upload, up, UP));
        (
            vec![
                row(0, target),
                row(4, chain),
                row(5, rule),
                row(3, process),
                row(7, traffic_line),
            ],
            Vec::new(),
        )
    }
}

fn draw_details(
    frame: &mut Frame,
    area: Rect,
    left: Vec<Line<'static>>,
    right: Vec<Line<'static>>,
) {
    let hint = format!(
        " ⏎ {}  y {}  d {} ",
        fl!("key-hide"),
        fl!("key-copy-target"),
        fl!("key-close")
    );
    let block = panel(&fl!("tui-panel-connection-details"), false)
        .title_top(Line::from(dim(hint)).right_aligned());
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if right.is_empty() {
        frame.render_widget(Paragraph::new(left), inner);
        return;
    }
    let [l, _, r] = Layout::horizontal([
        Constraint::Fill(1),
        Constraint::Length(2),
        Constraint::Fill(1),
    ])
    .areas(inner);
    frame.render_widget(Paragraph::new(left), l);
    frame.render_widget(Paragraph::new(right), r);
}

/// Footer hints for the tab.
pub fn hints(app: &App) -> Vec<(&'static str, String)> {
    let view = &app.connections;
    vec![
        ("↑↓", fl!("key-move")),
        (
            "⏎",
            if view.hide_details {
                fl!("key-details")
            } else {
                fl!("key-hide")
            },
        ),
        ("/", fl!("key-filter")),
        ("o", fl!("key-sort")),
        (
            "p",
            if view.paused {
                fl!("key-resume")
            } else {
                fl!("key-pause")
            },
        ),
        ("d", fl!("key-close")),
        (
            "D",
            if view.filter.is_empty() {
                fl!("key-close-all")
            } else {
                fl!("key-close-shown")
            },
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn connection(id: &str, host: &str, start: &str, bytes: u64) -> Connection {
        let mut c = Connection {
            id: id.to_owned(),
            start: start.to_owned(),
            upload: bytes,
            download: bytes,
            chains: vec!["node".to_owned(), "proxy".to_owned()],
            ..Connection::default()
        };
        c.metadata.host = host.to_owned();
        c.metadata.network = "tcp".to_owned();
        c.metadata.destination_port = "443".to_owned();
        c
    }

    fn shown(view: &ConnectionsView) -> Vec<&str> {
        view.rows
            .iter()
            .map(|&i| view.list[i].id.as_str())
            .collect()
    }

    fn sample() -> Vec<Connection> {
        vec![
            connection("a", "b.example", "2026-10-05T00:00:01Z", 30),
            connection("b", "a.example", "2026-10-05T00:00:03Z", 10),
            connection("c", "c.test", "2026-10-05T00:00:02Z", 20),
        ]
    }

    #[test]
    fn sorts_filters_and_keeps_the_cursor() {
        let mut view = ConnectionsView::default();
        view.take(sample());
        assert_eq!(shown(&view), ["b", "c", "a"]);
        view.state.select(Some(1));
        view.cycle_sort(); // rate: all idle, newest first
        view.cycle_sort(); // traffic
        assert_eq!(shown(&view), ["a", "c", "b"]);
        assert_eq!(view.selected().map(|c| c.id.as_str()), Some("c"));
        view.cycle_sort(); // host
        assert_eq!(shown(&view), ["b", "a", "c"]);
        view.toggle_reverse();
        assert_eq!(shown(&view), ["c", "a", "b"]);

        view.set_filter(" EXAMPLE  b. ");
        assert_eq!(view.filter, "EXAMPLE  b.");
        assert_eq!(shown(&view), ["a"]);
        assert_eq!(view.selected().map(|c| c.id.as_str()), Some("a"));
        view.set_filter("node tcp");
        assert_eq!(view.rows.len(), 3);
        view.set_filter("nothing");
        assert!(view.rows.is_empty() && view.selected().is_none());
    }

    #[test]
    fn rates_and_pause() {
        let mut view = ConnectionsView::default();
        view.take(sample());
        assert_eq!(view.rate("a"), (0, 0));
        let mut next = sample();
        next[0].download += 1000;
        next.push(connection("d", "d.test", "2026-10-05T00:00:04Z", 5));
        view.paused = true;
        view.take(next);
        // Paused: the old list stays, the count is live.
        assert_eq!(view.list.len(), 3);
        assert_eq!(view.live_len(), 4);
        view.toggle_pause();
        assert_eq!(view.list.len(), 4);
        let (up, down) = view.rate("a");
        assert!(up == 0 && down > 0);
        assert!(view.rate("d").1 > 0, "new connections count from zero");
        assert_eq!(view.busiest().map(|c| c.id.as_str()), Some("a"));
        view.clear();
        assert!(!view.loaded && view.list.is_empty());
    }

    #[test]
    fn chains_keep_the_node() {
        let groups = ["ai", "japan"];
        let fit = |width| fit_chain(&groups, "node", width);
        // ai → japan → node is 17 columns.
        assert_eq!(fit(17), (vec!["ai", "japan"], "node".to_owned()));
        assert_eq!(fit(16), (vec!["ai", "…"], "node".to_owned()));
        assert_eq!(fit(12), (vec!["ai"], "node".to_owned()));
        assert_eq!(fit(6), (vec![], "node".to_owned()));
        assert_eq!(fit(3), (vec![], "no…".to_owned()));
        assert_eq!(fit_chain(&[], "node", 4), (vec![], "node".to_owned()));
    }

    #[test]
    fn narrow_tables_drop_columns() {
        assert_eq!(columns(200, 5, true), Column::ALL);
        assert!(!columns(200, 5, false).contains(&Column::Process));
        let narrow = columns(60, 5, true);
        assert!(narrow.starts_with(&[Column::Activity, Column::Target]));
        assert!(narrow.contains(&Column::DownRate));
        assert!(!narrow.contains(&Column::Upload));
    }
}
