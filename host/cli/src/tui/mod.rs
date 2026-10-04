//! Terminal UI: session status, transfer progress, the device console and a log.

mod app;
mod console;
mod input;
mod ui;

use crossterm::event::{Event as TermEvent, EventStream};
use futures::StreamExt;
use ratatui::DefaultTerminal;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

use crate::transport::Transport;
use crate::worker::{self, Command, Event, Settings};
use app::App;

pub async fn run(transport: Transport, settings: Settings, port: String) -> anyhow::Result<()> {
    let app = App::new(
        port,
        settings.image.display().to_string(),
        settings.addr,
        settings.auto,
    );

    let (event_tx, events) = mpsc::unbounded_channel();
    let (commands, command_rx) = mpsc::unbounded_channel();
    tokio::spawn(worker::run(transport, settings, event_tx, command_rx));

    let mut terminal = ratatui::init();
    let result = event_loop(&mut terminal, app, events, commands).await;
    ratatui::restore();
    result
}

async fn event_loop(
    terminal: &mut DefaultTerminal,
    mut app: App,
    mut events: UnboundedReceiver<Event>,
    commands: UnboundedSender<Command>,
) -> anyhow::Result<()> {
    let mut terminal_events = EventStream::new();
    let mut worker_running = true;

    loop {
        terminal.draw(|frame| ui::draw(frame, &app))?;

        tokio::select! {
            event = events.recv(), if worker_running => match event {
                Some(event) => app.handle_worker_event(event),
                None => worker_running = false,
            },
            event = terminal_events.next() => match event {
                Some(Ok(TermEvent::Key(key))) => {
                    for command in app.handle_key(key) {
                        let _ = commands.send(command);
                    }
                }
                Some(Ok(_)) => {}
                Some(Err(e)) => return Err(e.into()),
                None => return Ok(()),
            },
        }

        // Handle everything that is already queued before drawing again
        while let Ok(event) = events.try_recv() {
            app.handle_worker_event(event);
        }

        if app.quit {
            return Ok(());
        }
    }
}
