//! Colours and building blocks shared by every view.
//!
//! The palette follows Catppuccin Mocha. Body text uses the terminal's own
//! foreground so the UI stays readable on light themes; colour carries
//! meaning (state, accents) and highlights carry their own fg/bg pair.

use ratatui::Frame;
use ratatui::layout::{Margin, Offset, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::symbols;
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, LineGauge, Padding, Scrollbar, ScrollbarOrientation, ScrollbarState, Shadow,
};

use super::mouse::{Pane, Parts, Target, hint_key};
use crate::protocol::CoreState;
use crate::util::pad;

pub const TEXT: Color = Color::Rgb(205, 214, 244);
pub const SUBTEXT: Color = Color::Rgb(166, 173, 200);
pub const DIM: Color = Color::Rgb(127, 132, 156);
pub const BORDER: Color = Color::Rgb(88, 91, 112);
pub const SURFACE: Color = Color::Rgb(49, 50, 68);
pub const SURFACE2: Color = Color::Rgb(69, 71, 90);
/// Background of the title and status bars.
pub const MANTLE: Color = Color::Rgb(24, 24, 37);
pub const CRUST: Color = Color::Rgb(17, 17, 27);
pub const ACCENT: Color = Color::Rgb(203, 166, 247);
pub const BLUE: Color = Color::Rgb(137, 180, 250);
pub const SKY: Color = Color::Rgb(137, 220, 235);
pub const TEAL: Color = Color::Rgb(148, 226, 213);
pub const GREEN: Color = Color::Rgb(166, 227, 161);
pub const YELLOW: Color = Color::Rgb(249, 226, 175);
pub const PEACH: Color = Color::Rgb(250, 179, 135);
pub const RED: Color = Color::Rgb(243, 139, 168);

/// Backgrounds of the text editor: the cursor line, the selection and
/// search matches.
pub const CURRENT_LINE: Color = Color::Rgb(40, 41, 59);
pub const SELECTION: Color = Color::Rgb(66, 78, 122);
pub const MATCH: Color = Color::Rgb(94, 84, 52);

/// Upload / download colours used for traffic everywhere.
pub const UP: Color = PEACH;
pub const DOWN: Color = TEAL;

pub const MARK: &str = "▍";
pub const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

pub fn state_color(state: CoreState) -> Color {
    match state {
        CoreState::Running => GREEN,
        CoreState::Starting | CoreState::Stopping | CoreState::Backoff => YELLOW,
        CoreState::Failed => RED,
        CoreState::Stopped => DIM,
    }
}

/// A filled label: ` TEXT ` on a coloured background.
pub fn pill(text: impl Into<String>, bg: Color) -> Span<'static> {
    Span::styled(
        format!(" {} ", text.into()),
        Style::new().fg(CRUST).bg(bg).add_modifier(Modifier::BOLD),
    )
}

/// A subtle label: coloured text on a dark chip.
pub fn chip(text: impl Into<String>, fg: Color) -> Span<'static> {
    Span::styled(
        format!(" {} ", text.into()),
        Style::new().fg(fg).bg(SURFACE),
    )
}

pub fn state_pill(state: CoreState) -> Span<'static> {
    pill(state.label(), state_color(state))
}

/// The state as a coloured dot and word, for places where a pill would
/// be too loud.
pub fn state_dot(state: CoreState) -> Vec<Span<'static>> {
    let color = state_color(state);
    vec![
        Span::styled("● ", Style::new().fg(color)),
        Span::styled(
            state.label(),
            Style::new().fg(color).add_modifier(Modifier::BOLD),
        ),
    ]
}

/// A key cap for hints: ` k `.
pub fn key(text: &str) -> Span<'static> {
    Span::styled(
        format!(" {text} "),
        Style::new()
            .fg(ACCENT)
            .bg(SURFACE)
            .add_modifier(Modifier::BOLD),
    )
}

pub fn dim(text: impl Into<String>) -> Span<'static> {
    Span::styled(text.into(), Style::new().fg(DIM))
}

pub fn panel(title: &str, focused: bool) -> Block<'static> {
    let border = if focused { ACCENT } else { BORDER };
    let title_style = if focused {
        Style::new().fg(CRUST).bg(ACCENT).bold()
    } else {
        Style::new().fg(ACCENT).bold()
    };
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(border))
        .title(Line::from(Span::styled(format!(" {title} "), title_style)))
        .padding(Padding::horizontal(1))
}

/// A small titled box, like the stats above the connections.
pub fn card(title: impl Into<String>, color: Color) -> Block<'static> {
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(BORDER))
        .title(Line::from(Span::styled(
            format!(" {} ", title.into()),
            Style::new().fg(color).add_modifier(Modifier::BOLD),
        )))
        .padding(Padding::horizontal(1))
}

/// The frame of a dialog: a coloured border, the title on a matching
/// pill and a shadow that lifts it off the dimmed screen behind.
pub fn dialog(title: &str, color: Color) -> Block<'static> {
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(color))
        .title(Line::from(Span::styled(
            format!(" {title} "),
            Style::new()
                .fg(CRUST)
                .bg(color)
                .add_modifier(Modifier::BOLD),
        )))
        .padding(Padding::horizontal(1))
        .shadow(
            Shadow::overlay()
                .style(Style::new().fg(SURFACE2).bg(CRUST))
                .offset(Offset::new(2, 1)),
        )
}

/// `k text` pairs for the bottom border of a dialog; a click on one
/// presses its key.
pub fn dialog_keys(keys: &[(&str, String)]) -> Parts {
    let mut parts = Parts::default();
    parts.text(Span::raw(" "));
    for (i, (k, text)) in keys.iter().enumerate() {
        if i > 0 {
            parts.text(Span::raw("  "));
        }
        parts.button(
            [
                key(k),
                Span::styled(format!(" {text}"), Style::new().fg(SUBTEXT)),
            ],
            hint_key(k).map(Target::Key),
        );
    }
    parts.text(Span::raw(" "));
    parts
}

/// Quiet `k text` pairs for the top border of a pane; a click on one
/// focuses the pane and presses its key.
pub fn pane_keys(pane: Option<Pane>, keys: &[(&str, String)]) -> Parts {
    let mut parts = Parts::default();
    parts.text(Span::raw(" "));
    for (i, (k, text)) in keys.iter().enumerate() {
        if i > 0 {
            parts.text(Span::raw("  "));
        }
        let target = hint_key(k).map(|key| match pane {
            Some(pane) => Target::PaneKey(pane, key),
            None => Target::Key(key),
        });
        parts.button([dim(format!("{k} {text}"))], target);
    }
    parts.text(Span::raw(" "));
    parts
}

/// Fades everything drawn so far, so that a dialog drawn next stands out.
pub fn backdrop(frame: &mut Frame) {
    for cell in &mut frame.buffer_mut().content {
        cell.fg = match cell.fg {
            Color::Rgb(r, g, b) => blend((r, g, b), (30, 30, 46), 0.62),
            _ => BORDER,
        };
        if let Color::Rgb(r, g, b) = cell.bg {
            cell.bg = blend((r, g, b), (17, 17, 27), 0.55);
        }
        cell.modifier.remove(Modifier::BOLD);
    }
}

fn blend(from: (u8, u8, u8), to: (u8, u8, u8), amount: f32) -> Color {
    let mix = |a: u8, b: u8| (f32::from(a) + (f32::from(b) - f32::from(a)) * amount) as u8;
    Color::Rgb(mix(from.0, to.0), mix(from.1, to.1), mix(from.2, to.2))
}

/// A thumb on the right border of the bordered `area` when `total` rows
/// do not fit the `viewport` rows shown from `offset`.
pub fn scrollbar(
    frame: &mut Frame,
    area: Rect,
    (total, offset, viewport): (usize, usize, usize),
    focused: bool,
) {
    if viewport == 0 || total <= viewport {
        return;
    }
    let mut state = ScrollbarState::new(total - viewport + 1)
        .position(offset)
        .viewport_content_length(viewport);
    frame.render_stateful_widget(
        Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(None)
            .end_symbol(None)
            .track_symbol(None)
            .thumb_symbol("┃")
            .thumb_style(Style::new().fg(if focused { ACCENT } else { DIM })),
        area.inner(Margin::new(0, 1)),
        &mut state,
    );
}

/// A one-line gauge: `label ━━━━━━────`.
pub fn meter(ratio: f64, color: Color, label: impl Into<Line<'static>>) -> LineGauge<'static> {
    LineGauge::default()
        .ratio(ratio.clamp(0.0, 1.0))
        .label(label)
        .filled_symbol(symbols::line::THICK_HORIZONTAL)
        .unfilled_symbol(symbols::line::HORIZONTAL)
        .filled_style(Style::new().fg(color))
        .unfilled_style(Style::new().fg(SURFACE2))
}

/// Green while there is room, then yellow, then red.
pub fn usage_color(ratio: f64) -> Color {
    match ratio {
        r if r >= 0.9 => RED,
        r if r >= 0.7 => YELLOW,
        _ => GREEN,
    }
}

/// Highlight of the selected table/list row.
pub fn selected(focused: bool) -> Style {
    if focused {
        Style::new()
            .fg(TEXT)
            .bg(SURFACE2)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::new().fg(TEXT).bg(SURFACE)
    }
}

pub fn header_row() -> Style {
    Style::new().fg(SUBTEXT).add_modifier(Modifier::BOLD)
}

/// Label cell of a key/value card, padded to `width` columns.
pub fn label(text: &str, width: usize) -> Span<'static> {
    Span::styled(pad(text, width), Style::new().fg(DIM))
}

/// `label  value` line for key/value cards.
pub fn field(text: &str, width: usize, value: impl Into<Span<'static>>) -> Line<'static> {
    Line::from(vec![label(text, width), value.into()])
}

/// Delay in milliseconds coloured by quality.
pub fn delay_color(ms: u32) -> Color {
    match ms {
        0..300 => GREEN,
        300..800 => YELLOW,
        _ => RED,
    }
}

/// Signal bars for a delay: four for a fast node, none for a timeout.
pub fn signal(delay: Option<Result<u32, ()>>) -> Vec<Span<'static>> {
    const BARS: [&str; 4] = ["▂", "▄", "▆", "█"];
    let (lit, color) = match delay {
        None => (0, DIM),
        Some(Err(())) => (0, RED),
        Some(Ok(ms)) => (
            match ms {
                0..150 => 4,
                150..300 => 3,
                300..800 => 2,
                _ => 1,
            },
            delay_color(ms),
        ),
    };
    BARS.iter()
        .enumerate()
        .map(|(i, bar)| {
            Span::styled(
                *bar,
                Style::new().fg(if i < lit { color } else { SURFACE2 }),
            )
        })
        .collect()
}
