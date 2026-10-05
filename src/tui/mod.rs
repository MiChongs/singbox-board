//! Terminal dashboard built on ratatui.

mod app;
mod core;
mod editor;
mod popup;
mod profiles;
mod tasks;
mod templates;
mod theme;
mod ui;

use std::io::Write;
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{
    DisableBracketedPaste, EnableBracketedPaste, Event, EventStream, KeyEventKind,
};
use crossterm::terminal::{Clear, ClearType, EnterAlternateScreen, enable_raw_mode};
use futures::StreamExt;
use tokio::sync::mpsc;

use self::app::App;
use crate::client::DaemonClient;

pub async fn run(client: DaemonClient) -> Result<()> {
    let mut terminal = ratatui::init();
    let _ = crossterm::execute!(std::io::stdout(), EnableBracketedPaste);
    let result = event_loop(&mut terminal, client).await;
    let _ = crossterm::execute!(std::io::stdout(), DisableBracketedPaste);
    ratatui::restore();
    result
}

async fn event_loop(terminal: &mut ratatui::DefaultTerminal, client: DaemonClient) -> Result<()> {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let (mut app, mut background) = App::new(client, tx);
    let mut events = EventStream::new();
    let mut tick = tokio::time::interval(Duration::from_millis(250));

    while !app.should_quit {
        terminal.draw(|frame| ui::draw(frame, &mut app))?;
        tokio::select! {
            event = events.next() => match event {
                Some(Ok(Event::Key(key))) if key.kind != KeyEventKind::Release => app.on_key(key),
                Some(Ok(Event::Paste(text))) => app.on_paste(&text),
                Some(Ok(_)) => {}
                Some(Err(err)) => return Err(err.into()),
                None => break,
            },
            Some(event) = rx.recv() => {
                app.on_event(event);
                // Apply bursts (e.g. log backlog) before the next redraw.
                while let Ok(event) = rx.try_recv() {
                    app.on_event(event);
                }
            }
            _ = tick.tick() => app.on_tick(),
        }
        if let Some(text) = app.take_clipboard() {
            copy_to_clipboard(&text)?;
        }
        if let Some(edit) = app.take_external_edit() {
            // The old stream would keep reading keys meant for the editor.
            events = EventStream::new();
            let result = run_editor(terminal, &edit.text, &edit.name)?;
            app.external_edit_done(edit, result);
        }
    }
    background.abort_all();
    Ok(())
}

/// Hands the terminal to `$EDITOR` and takes it back afterwards.
fn run_editor(
    terminal: &mut ratatui::DefaultTerminal,
    text: &str,
    name: &str,
) -> Result<anyhow::Result<String>> {
    let _ = crossterm::execute!(std::io::stdout(), DisableBracketedPaste);
    ratatui::restore();
    let result = tokio::task::block_in_place(|| crate::util::edit_text(text, name));
    enable_raw_mode()?;
    crossterm::execute!(
        std::io::stdout(),
        EnterAlternateScreen,
        Clear(ClearType::All),
        EnableBracketedPaste
    )?;
    // A new terminal repaints everything. `Terminal::clear` would do the
    // same but asks the terminal for the cursor position, which not every
    // terminal answers.
    *terminal = ratatui::Terminal::new(ratatui::backend::CrosstermBackend::new(std::io::stdout()))?;
    Ok(result)
}

/// Sets the system clipboard through the terminal (OSC 52); supported by most
/// modern terminals, including over SSH and inside tmux with `set-clipboard on`.
fn copy_to_clipboard(text: &str) -> Result<()> {
    use base64::Engine;
    let encoded = base64::engine::general_purpose::STANDARD.encode(text);
    let mut stdout = std::io::stdout();
    write!(stdout, "\x1b]52;c;{encoded}\x07")?;
    stdout.flush()?;
    Ok(())
}
