//! Terminal dashboard built on ratatui.

mod app;
mod buffer;
mod code;
mod core;
mod editor;
mod jsonc;
mod popup;
mod profiles;
mod tasks;
mod templates;
mod theme;
mod ui;

use std::io::Write;
use std::process::{Command, Stdio};
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
use crate::protocol::Profile;

pub async fn run(client: DaemonClient) -> Result<()> {
    session(client, None).await.map(drop)
}

/// Opens one profile in the editor and returns once it is closed, with
/// the message of the last save.
pub async fn edit(
    client: DaemonClient,
    profile: Profile,
    content: String,
    force: bool,
) -> Result<Option<String>> {
    session(client, Some((profile, content, force))).await
}

async fn session(
    client: DaemonClient,
    edit: Option<(Profile, String, bool)>,
) -> Result<Option<String>> {
    let mut terminal = ratatui::init();
    let _ = crossterm::execute!(std::io::stdout(), EnableBracketedPaste);
    let mut mouse = false;
    let result = event_loop(&mut terminal, client, edit, &mut mouse).await;
    set_mouse(&mut mouse, false);
    let _ = crossterm::execute!(std::io::stdout(), DisableBracketedPaste);
    ratatui::restore();
    result
}

async fn event_loop(
    terminal: &mut ratatui::DefaultTerminal,
    client: DaemonClient,
    edit: Option<(Profile, String, bool)>,
    mouse: &mut bool,
) -> Result<Option<String>> {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let (mut app, mut background) = App::new(client, tx);
    if let Some((profile, content, force)) = edit {
        app.start_editing(profile, &content, force);
    }
    let mut events = EventStream::new();
    let mut tick = tokio::time::interval(Duration::from_millis(250));

    while !app.should_quit {
        set_mouse(mouse, app.wants_mouse());
        terminal.draw(|frame| ui::draw(frame, &mut app))?;
        tokio::select! {
            event = events.next() => match event {
                Some(Ok(Event::Key(key))) if key.kind != KeyEventKind::Release => app.on_key(key),
                Some(Ok(Event::Paste(text))) => app.on_paste(&text),
                Some(Ok(Event::Mouse(event))) => app.on_mouse(event),
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
            set_mouse(mouse, false);
            let result = run_editor(terminal, &edit.text, &edit.name)?;
            app.external_edit_done(edit, result);
        }
    }
    background.abort_all();
    Ok(app.take_exit_message())
}

/// Turns mouse reporting on or off: clicks, drags and the wheel, but not
/// plain motion, which would only cost redraws.
fn set_mouse(on: &mut bool, want: bool) {
    if *on == want {
        return;
    }
    let sequence = if want {
        "\x1b[?1000h\x1b[?1002h\x1b[?1006h"
    } else {
        "\x1b[?1006l\x1b[?1002l\x1b[?1000l"
    };
    let mut stdout = std::io::stdout();
    if stdout
        .write_all(sequence.as_bytes())
        .and_then(|()| stdout.flush())
        .is_ok()
    {
        *on = want;
    }
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

/// Sets the system clipboard through the terminal (OSC 52), which works in
/// most modern terminals, over SSH and inside tmux with `set-clipboard on`,
/// and through wl-copy, xclip or xsel on a local desktop for terminals
/// without OSC 52.
fn copy_to_clipboard(text: &str) -> Result<()> {
    use base64::Engine;
    let encoded = base64::engine::general_purpose::STANDARD.encode(text);
    let mut stdout = std::io::stdout();
    write!(stdout, "\x1b]52;c;{encoded}\x07")?;
    stdout.flush()?;
    copy_with_tool(text);
    Ok(())
}

fn copy_with_tool(text: &str) {
    let mut tools: Vec<(&str, &[&str])> = Vec::new();
    if std::env::var_os("WAYLAND_DISPLAY").is_some() {
        tools.push(("wl-copy", &[]));
    }
    if std::env::var_os("DISPLAY").is_some() {
        tools.push(("xclip", &["-selection", "clipboard"]));
        tools.push(("xsel", &["--clipboard", "--input"]));
    }
    for (program, args) in tools {
        let Ok(mut child) = Command::new(program)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        else {
            continue;
        };
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(text.as_bytes());
        }
        // These fork to serve the clipboard; reap the parent off the UI thread.
        std::thread::spawn(move || {
            let _ = child.wait();
        });
        return;
    }
}
