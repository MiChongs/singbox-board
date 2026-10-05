//! Terminal dashboard built on ratatui.

mod app;
mod tasks;
mod ui;

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
    }
    background.abort_all();
    Ok(())
}
