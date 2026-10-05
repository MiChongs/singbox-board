//! Terminal dashboard built on ratatui.

mod app;
mod core;
mod tasks;
mod theme;
mod ui;

use std::io::Write;
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{Event, EventStream, KeyEventKind};
use futures::StreamExt;
use tokio::sync::mpsc;

use self::app::App;
use crate::client::DaemonClient;

pub async fn run(client: DaemonClient) -> Result<()> {
    let mut terminal = ratatui::init();
    let result = event_loop(&mut terminal, client).await;
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
    }
    background.abort_all();
    Ok(())
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
