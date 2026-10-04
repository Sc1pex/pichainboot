//! Runs sessions with the device in the background: connects, loads and boots
//! the image when asked to, then acts as a console until the device resets.
//! The frontends (headless and TUI) talk to it through channels.

use std::path::PathBuf;
use std::time::Duration;

use pichainboot_proto::frame::{MAGIC, MagicMatcher};
use pichainboot_proto::{BootOutcome, Client, Config, Error, Info};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

use crate::transport::{Reset, Transport};

/// The device ends a session after 10 s without a valid frame
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(3);

#[derive(Debug, Clone)]
pub struct Settings {
    pub image: PathBuf,
    pub addr: u64,
    pub entry: u64,
    pub client: Config,
    /// Boot whenever the device connects
    pub auto: bool,
    /// Boot as soon as the device connects until the first boot succeeds,
    /// even without `auto`
    pub boot_first: bool,
    /// How to reset the device, if the connection supports it
    pub reset: Option<Reset>,
    /// Reset the device before waiting for it
    pub reset_on_start: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    WaitingForDevice,
    Connected(Info),
    Loading,
    Booted(BootOutcome),
}

#[derive(Debug)]
pub enum Event {
    Status(Status),
    Progress { sent: usize, total: usize },
    Console(Vec<u8>),
    Log(String),
    Error(String),
    /// The worker stopped
    Fatal(String),
}

#[derive(Debug)]
pub enum Command {
    Boot,
    SetAuto(bool),
    /// Reset the device through the connection
    Reset,
    /// Console input for the booted kernel
    Input(Vec<u8>),
}

pub async fn run(transport: Transport, settings: Settings, events: UnboundedSender<Event>, commands: UnboundedReceiver<Command>) {
    let mut worker = Worker {
        settings,
        events,
        commands,
    };
    if let Err(e) = worker.run(transport).await {
        worker.send(Event::Fatal(e.to_string()));
    }
}

struct Worker {
    settings: Settings,
    events: UnboundedSender<Event>,
    commands: UnboundedReceiver<Command>,
}

/// What ended the console phase
enum ConsoleEnd {
    /// The device reset and is announcing itself. Holds the bytes that followed
    /// the magic sequence.
    DeviceReset(Vec<u8>),
    /// The frontend went away
    Quit,
}

impl Worker {
    fn send(&self, event: Event) {
        // The frontend going away is handled through the command channel
        let _ = self.events.send(event);
    }

    fn log(&self, message: impl Into<String>) {
        self.send(Event::Log(message.into()));
    }

    async fn reset_device(&self) {
        let Some(reset) = &self.settings.reset else {
            self.send(Event::Error("this connection cannot reset the device".into()));
            return;
        };
        self.log("resetting the device");
        if let Err(e) = reset.reset().await {
            self.send(Event::Error(format!("reset failed: {e:#}")));
        }
    }

    async fn run(&mut self, mut io: Transport) -> Result<(), Error> {
        let mut pending = Vec::new();
        let mut booted_once = false;

        if self.settings.reset_on_start {
            self.reset_device().await;
        }

        loop {
            let mut client = Client::with_pending(io, self.settings.client.clone(), pending);
            self.send(Event::Status(Status::WaitingForDevice));
            let Some(info) = self.connect(&mut client).await? else {
                return Ok(());
            };
            self.send(Event::Status(Status::Connected(info)));
            self.log(format!(
                "connected: protocol v{}, max payload {} bytes",
                info.version, info.max_payload
            ));

            let boot_now = self.settings.auto || (!booted_once && self.settings.boot_first);
            match self.session(&mut client, boot_now).await? {
                SessionEnd::Booted => booted_once = true,
                SessionEnd::Reconnect => {
                    (io, pending) = client.into_parts();
                    continue;
                }
                SessionEnd::Quit => return Ok(()),
            }

            let (stream, leftover) = client.into_parts();
            io = stream;
            match self.console(&mut io, leftover).await? {
                ConsoleEnd::DeviceReset(rest) => {
                    self.log("device reset");
                    pending = rest;
                }
                ConsoleEnd::Quit => return Ok(()),
            }
        }
    }

    /// Waits for the device, returns None if the frontend went away
    async fn connect(&mut self, client: &mut Client<Transport>) -> Result<Option<Info>, Error> {
        loop {
            tokio::select! {
                result = client.connect() => return result.map(Some),
                command = self.commands.recv() => match command {
                    Some(Command::SetAuto(auto)) => self.settings.auto = auto,
                    Some(Command::Boot) => self.log("waiting for the device, reset it to connect"),
                    Some(Command::Reset) => self.reset_device().await,
                    Some(Command::Input(_)) => {}
                    None => return Ok(None),
                },
            }
        }
    }

    /// Waits for a boot request and boots, keeping the session alive meanwhile
    async fn session(&mut self, client: &mut Client<Transport>, mut boot_now: bool) -> Result<SessionEnd, Error> {
        loop {
            if !boot_now {
                let mut keepalive = tokio::time::interval(KEEPALIVE_INTERVAL);
                keepalive.reset();
                loop {
                    tokio::select! {
                        command = self.commands.recv() => match command {
                            Some(Command::Boot) => break,
                            Some(Command::SetAuto(auto)) => {
                                self.settings.auto = auto;
                                if auto {
                                    break;
                                }
                            }
                            // The device announces itself again after the
                            // reset and the session restarts
                            Some(Command::Reset) => self.reset_device().await,
                            Some(Command::Input(_)) => {}
                            None => return Ok(SessionEnd::Quit),
                        },
                        _ = keepalive.tick() => match client.ping().await {
                            Ok(()) => {}
                            Err(Error::SessionRestarted) => self.log("device reset, new session started"),
                            Err(Error::Timeout(..)) => {
                                self.send(Event::Error("device stopped responding".into()));
                                return Ok(SessionEnd::Reconnect);
                            }
                            Err(e) => return Err(e),
                        },
                    }
                }
            }
            // After a failure, wait for the user instead of retrying in a loop
            boot_now = false;

            let image = match tokio::fs::read(&self.settings.image).await {
                Ok(image) => image,
                Err(e) => {
                    self.send(Event::Error(format!("failed to read {}: {e}", self.settings.image.display())));
                    continue;
                }
            };

            self.send(Event::Status(Status::Loading));
            self.log(format!(
                "loading {} ({} bytes) at {:#x}",
                self.settings.image.display(),
                image.len(),
                self.settings.addr
            ));
            let events = self.events.clone();
            let mut last_percent = None;
            let progress = |sent: usize, total: usize| {
                let percent = sent * 100 / total;
                if last_percent != Some(percent) {
                    last_percent = Some(percent);
                    let _ = events.send(Event::Progress { sent, total });
                }
            };

            // Keep handling commands while loading. A reset makes the device
            // restart the session, and the load starts over.
            let result = {
                let load = client.load_and_boot(self.settings.addr, self.settings.entry, &image, progress);
                tokio::pin!(load);
                loop {
                    tokio::select! {
                        result = &mut load => break result,
                        command = self.commands.recv() => match command {
                            Some(Command::Reset) => self.reset_device().await,
                            Some(Command::SetAuto(auto)) => self.settings.auto = auto,
                            Some(Command::Boot | Command::Input(_)) => {}
                            None => return Ok(SessionEnd::Quit),
                        },
                    }
                }
            };

            match result {
                Ok(outcome) => {
                    match outcome {
                        BootOutcome::Confirmed => self.log(format!("booted at {:#x}", self.settings.entry)),
                        BootOutcome::Unconfirmed => {
                            self.send(Event::Error("the device did not confirm the boot".into()));
                        }
                    }
                    self.send(Event::Status(Status::Booted(outcome)));
                    return Ok(SessionEnd::Booted);
                }
                Err(Error::Timeout(..) | Error::SessionRestarted) => {
                    self.send(Event::Error("lost the device while loading".into()));
                    return Ok(SessionEnd::Reconnect);
                }
                Err(e @ (Error::Device { .. } | Error::EmptyImage | Error::ImageTooLarge(_))) => {
                    self.send(Event::Error(format!("load failed: {e}")));
                    if let Some(info) = client.info() {
                        self.send(Event::Status(Status::Connected(info)));
                    }
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// Forwards console output and input until the device resets
    async fn console(&mut self, io: &mut Transport, leftover: Vec<u8>) -> Result<ConsoleEnd, Error> {
        let mut magic = MagicMatcher::default();
        if let Some(end) = self.console_output(&mut magic, &leftover) {
            return Ok(end);
        }

        let mut buf = [0; 4096];
        loop {
            tokio::select! {
                n = io.read(&mut buf) => {
                    let n = n?;
                    if n == 0 {
                        return Err(Error::Closed);
                    }
                    if let Some(end) = self.console_output(&mut magic, &buf[..n]) {
                        return Ok(end);
                    }
                }
                command = self.commands.recv() => match command {
                    Some(Command::Input(bytes)) => {
                        io.write_all(&bytes).await?;
                        io.flush().await?;
                    }
                    Some(Command::SetAuto(auto)) => self.settings.auto = auto,
                    Some(Command::Boot) => self.log("reset the device to boot again"),
                    Some(Command::Reset) => self.reset_device().await,
                    None => return Ok(ConsoleEnd::Quit),
                },
            }
        }
    }

    /// Passes console output on, watching for the device announcing itself
    fn console_output(&self, magic: &mut MagicMatcher, bytes: &[u8]) -> Option<ConsoleEnd> {
        for (i, &byte) in bytes.iter().enumerate() {
            if magic.feed(byte) {
                // Leave the magic sequence out of the console output
                let magic_start = (i + 1).saturating_sub(MAGIC.len());
                if magic_start > 0 {
                    self.send(Event::Console(bytes[..magic_start].to_vec()));
                }
                return Some(ConsoleEnd::DeviceReset(bytes[i + 1..].to_vec()));
            }
        }
        if !bytes.is_empty() {
            self.send(Event::Console(bytes.to_vec()));
        }
        None
    }
}

enum SessionEnd {
    Booted,
    Reconnect,
    Quit,
}
