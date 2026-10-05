//! Colours and building blocks shared by every view.
//!
//! The palette follows Catppuccin Mocha. Body text uses the terminal's own
//! foreground so the UI stays readable on light themes; colour carries
//! meaning (state, accents) and highlights carry their own fg/bg pair.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Padding};

use crate::protocol::CoreState;
use crate::util::pad;

pub const TEXT: Color = Color::Rgb(205, 214, 244);
pub const SUBTEXT: Color = Color::Rgb(166, 173, 200);
pub const DIM: Color = Color::Rgb(127, 132, 156);
pub const BORDER: Color = Color::Rgb(88, 91, 112);
pub const SURFACE: Color = Color::Rgb(49, 50, 68);
pub const SURFACE2: Color = Color::Rgb(69, 71, 90);
pub const CRUST: Color = Color::Rgb(17, 17, 27);
pub const ACCENT: Color = Color::Rgb(203, 166, 247);
pub const BLUE: Color = Color::Rgb(137, 180, 250);
pub const SKY: Color = Color::Rgb(137, 220, 235);
pub const TEAL: Color = Color::Rgb(148, 226, 213);
pub const GREEN: Color = Color::Rgb(166, 227, 161);
pub const YELLOW: Color = Color::Rgb(249, 226, 175);
pub const PEACH: Color = Color::Rgb(250, 179, 135);
pub const RED: Color = Color::Rgb(243, 139, 168);

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
