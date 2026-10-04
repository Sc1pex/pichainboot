//! Frontend without the TUI: status on stderr, then a plain serial console on
//! stdin/stdout. When stdin is a terminal it is put in raw mode and Ctrl-]
//! quits.

use std::io::{IsTerminal, Write};

use anyhow::anyhow;
use crossterm::terminal;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;

use crate::transport::Transport;
use crate::worker::{self, Command, Event, Settings, Status};

const QUIT_KEY: u8 = 0x1d; // Ctrl-]

struct RawMode;

impl RawMode {
    fn enable() -> anyhow::Result<Self> {
        terminal::enable_raw_mode()?;
        Ok(Self)
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        let _ = terminal::disable_raw_mode();
    }
}

/// Prints status lines between console output. Raw mode needs explicit
/// carriage returns.
#[derive(Default)]
struct StatusPrinter {
    /// The console output so far ended in the middle of a line
    mid_line: bool,
}

impl StatusPrinter {
    fn console(&mut self, bytes: &[u8]) {
        if let Some(&last) = bytes.last() {
            self.mid_line = last != b'\n';
        }
    }

    fn line(&mut self, message: &str) {
        let mut stderr = std::io::stderr();
        let start = if std::mem::take(&mut self.mid_line) { "\r\n" } else { "\r" };
        let _ = write!(stderr, "{start}[pichainboot] {message}\r\n");
        let _ = stderr.flush();
    }
}

pub async fn run(transport: Transport, settings: Settings, notes: Vec<String>) -> anyhow::Result<()> {
    let (event_tx, mut events) = mpsc::unbounded_channel();
    let (commands, command_rx) = mpsc::unbounded_channel();
    tokio::spawn(worker::run(transport, settings, event_tx, command_rx));

    let mut status = StatusPrinter::default();
    for note in &notes {
        status.line(note);
    }
    let interactive = std::io::stdin().is_terminal();
    let _raw_mode = if interactive {
        status.line("press Ctrl-] to quit");
        Some(RawMode::enable()?)
    } else {
        None
    };

    let (quit_tx, mut quit) = mpsc::channel::<()>(1);
    tokio::spawn(async move {
        let mut stdin = tokio::io::stdin();
        let mut buf = [0; 256];
        loop {
            let n = match stdin.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            let input = &buf[..n];
            if interactive && let Some(pos) = input.iter().position(|&b| b == QUIT_KEY) {
                let _ = commands.send(Command::Input(input[..pos].to_vec()));
                let _ = quit_tx.send(()).await;
                return;
            }
            if commands.send(Command::Input(input.to_vec())).is_err() {
                return;
            }
        }
        // Dropping the sender would stop the worker, keep showing the console
        // until Ctrl-C instead
        let _commands = commands;
        std::future::pending::<()>().await;
    });

    let mut stdout = tokio::io::stdout();
    loop {
        let event = tokio::select! {
            event = events.recv() => event,
            _ = quit.recv() => return Ok(()),
            _ = tokio::signal::ctrl_c() => return Ok(()),
        };
        match event {
            None => return Ok(()),
            Some(Event::Console(bytes)) => {
                status.console(&bytes);
                stdout.write_all(&bytes).await?;
                stdout.flush().await?;
            }
            Some(Event::Status(Status::WaitingForDevice)) => status.line("waiting for the device"),
            Some(Event::Status(_)) => {}
            Some(Event::Progress { sent, total }) => {
                let mut stderr = std::io::stderr();
                let _ = write!(stderr, "\r[pichainboot] loading {}%", sent * 100 / total);
                if sent == total {
                    let _ = write!(stderr, "\r\n");
                }
                let _ = stderr.flush();
            }
            Some(Event::Log(message)) => status.line(&message),
            Some(Event::Error(message)) => status.line(&format!("error: {message}")),
            Some(Event::Fatal(message)) => return Err(anyhow!(message)),
        }
    }
}
