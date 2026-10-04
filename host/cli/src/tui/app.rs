use std::collections::VecDeque;
use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use super::console::Console;
use super::input::key_to_bytes;
use crate::worker::{Command, Event, Status};

const MAX_LOG_LINES: usize = 200;
/// Ctrl-A, like screen and picocom
const PREFIX_KEY: char = 'a';

#[derive(Debug)]
pub struct LogLine {
    pub elapsed_secs: f64,
    pub text: String,
    pub error: bool,
}

#[derive(Debug)]
pub struct App {
    pub port: String,
    pub image: String,
    pub addr: u64,
    pub auto: bool,
    /// The connection can reset the device
    pub can_reset: bool,
    pub status: Status,
    pub progress: Option<(usize, usize)>,
    /// Set when the worker stopped
    pub fatal: Option<String>,
    pub console: Console,
    pub log: VecDeque<LogLine>,
    /// How many lines the console is scrolled up from the bottom
    pub scroll: usize,
    /// Ctrl-A was pressed and the next key is a command
    pub prefix: bool,
    pub quit: bool,
    started: Instant,
}

impl App {
    pub fn new(port: String, image: String, addr: u64, auto: bool) -> Self {
        Self {
            port,
            image,
            addr,
            auto,
            can_reset: false,
            status: Status::WaitingForDevice,
            progress: None,
            fatal: None,
            console: Console::default(),
            log: VecDeque::new(),
            scroll: 0,
            prefix: false,
            quit: false,
            started: Instant::now(),
        }
    }

    /// True once a kernel was booted and the console is live
    pub fn console_active(&self) -> bool {
        matches!(self.status, Status::Booted(_)) && self.fatal.is_none()
    }

    fn push_log(&mut self, text: String, error: bool) {
        self.log.push_back(LogLine {
            elapsed_secs: self.started.elapsed().as_secs_f64(),
            text,
            error,
        });
        if self.log.len() > MAX_LOG_LINES {
            self.log.pop_front();
        }
    }

    pub fn handle_worker_event(&mut self, event: Event) {
        match event {
            Event::Status(status) => {
                if status != Status::Loading {
                    self.progress = None;
                }
                if matches!(self.status, Status::Booted(_)) && !matches!(status, Status::Booted(_)) {
                    // The kernel is gone, output from the next one starts on a new line
                    self.console.finish_line();
                }
                self.status = status;
            }
            Event::Progress { sent, total } => self.progress = Some((sent, total)),
            Event::Console(bytes) => self.console.push(&bytes),
            Event::Log(text) => self.push_log(text, false),
            Event::Error(text) => self.push_log(text, true),
            Event::Fatal(text) => {
                self.push_log(text.clone(), true);
                self.fatal = Some(text);
            }
        }
    }

    /// Handles a key press, returns the commands to send to the worker
    pub fn handle_key(&mut self, key: KeyEvent) -> Vec<Command> {
        if key.kind != KeyEventKind::Press {
            return Vec::new();
        }

        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if std::mem::take(&mut self.prefix) {
            return match key.code {
                // Ctrl-A twice sends a Ctrl-A
                KeyCode::Char(PREFIX_KEY) if ctrl && self.console_active() => vec![Command::Input(vec![0x01])],
                KeyCode::Char(c) => self.command(c),
                _ => Vec::new(),
            };
        }
        if ctrl && key.code == KeyCode::Char(PREFIX_KEY) {
            self.prefix = true;
            return Vec::new();
        }

        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let console_active = self.console_active();
        match key.code {
            KeyCode::PageUp if shift || !console_active => self.scroll_by(10),
            KeyCode::PageDown if shift || !console_active => self.scroll_by(-10),
            _ if console_active => {
                self.scroll = 0;
                return key_to_bytes(key).map(Command::Input).into_iter().collect();
            }
            KeyCode::Char('c') if ctrl => self.quit = true,
            KeyCode::Char(c) if !ctrl => return self.command(c),
            _ => {}
        }
        Vec::new()
    }

    fn command(&mut self, c: char) -> Vec<Command> {
        match c {
            'b' => return vec![Command::Boot],
            'a' => {
                self.auto = !self.auto;
                let state = if self.auto { "on" } else { "off" };
                self.push_log(format!("auto boot {state}"), false);
                return vec![Command::SetAuto(self.auto)];
            }
            'r' if self.can_reset => return vec![Command::Reset],
            'r' => self.push_log("this connection cannot reset the device".into(), true),
            'c' => {
                self.console.clear();
                self.scroll = 0;
            }
            'q' => self.quit = true,
            _ => {}
        }
        Vec::new()
    }

    fn scroll_by(&mut self, lines: isize) {
        let max = self.console.lines().len().saturating_sub(1);
        self.scroll = self.scroll.saturating_add_signed(lines).min(max);
    }
}

#[cfg(test)]
mod tests {
    use pichainboot_proto::BootOutcome;

    use super::*;

    fn app() -> App {
        App::new("/dev/ttyUSB0".into(), "kernel8.img".into(), 0x80000, false)
    }

    fn press(app: &mut App, code: KeyCode, modifiers: KeyModifiers) -> Vec<Command> {
        app.handle_key(KeyEvent::new(code, modifiers))
    }

    fn inputs(commands: &[Command]) -> Vec<u8> {
        commands
            .iter()
            .flat_map(|c| match c {
                Command::Input(bytes) => bytes.clone(),
                other => panic!("unexpected command {other:?}"),
            })
            .collect()
    }

    #[test]
    fn plain_keys_are_commands_before_boot() {
        let mut app = app();
        assert!(matches!(press(&mut app, KeyCode::Char('b'), KeyModifiers::NONE)[..], [Command::Boot]));
        assert!(matches!(
            press(&mut app, KeyCode::Char('a'), KeyModifiers::NONE)[..],
            [Command::SetAuto(true)]
        ));
        press(&mut app, KeyCode::Char('q'), KeyModifiers::NONE);
        assert!(app.quit);
    }

    #[test]
    fn keys_go_to_the_console_after_boot() {
        let mut app = app();
        app.handle_worker_event(Event::Status(Status::Booted(BootOutcome::Confirmed)));

        let commands = press(&mut app, KeyCode::Char('q'), KeyModifiers::NONE);
        assert_eq!(inputs(&commands), b"q");
        assert!(!app.quit);
        let commands = press(&mut app, KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(inputs(&commands), [0x03]);
        assert!(!app.quit);
    }

    #[test]
    fn prefix_key_runs_commands_in_the_console() {
        let mut app = app();
        app.handle_worker_event(Event::Status(Status::Booted(BootOutcome::Confirmed)));

        assert!(press(&mut app, KeyCode::Char('a'), KeyModifiers::CONTROL).is_empty());
        assert!(matches!(press(&mut app, KeyCode::Char('b'), KeyModifiers::NONE)[..], [Command::Boot]));

        press(&mut app, KeyCode::Char('a'), KeyModifiers::CONTROL);
        let commands = press(&mut app, KeyCode::Char('a'), KeyModifiers::CONTROL);
        assert_eq!(inputs(&commands), [0x01]);

        press(&mut app, KeyCode::Char('a'), KeyModifiers::CONTROL);
        press(&mut app, KeyCode::Char('q'), KeyModifiers::NONE);
        assert!(app.quit);
    }

    #[test]
    fn resets_only_when_the_connection_can() {
        let mut app = app();
        assert!(press(&mut app, KeyCode::Char('r'), KeyModifiers::NONE).is_empty());
        assert!(app.log.back().unwrap().error);

        app.can_reset = true;
        assert!(matches!(press(&mut app, KeyCode::Char('r'), KeyModifiers::NONE)[..], [Command::Reset]));
        app.handle_worker_event(Event::Status(Status::Booted(BootOutcome::Confirmed)));
        press(&mut app, KeyCode::Char('a'), KeyModifiers::CONTROL);
        assert!(matches!(press(&mut app, KeyCode::Char('r'), KeyModifiers::NONE)[..], [Command::Reset]));
    }

    #[test]
    fn scrolls_within_the_scrollback() {
        let mut app = app();
        app.console.push(b"1\n2\n3\n");
        press(&mut app, KeyCode::PageUp, KeyModifiers::NONE);
        assert_eq!(app.scroll, 3);
        press(&mut app, KeyCode::PageDown, KeyModifiers::NONE);
        assert_eq!(app.scroll, 0);
    }

    #[test]
    fn tracks_worker_events() {
        let mut app = app();
        app.handle_worker_event(Event::Status(Status::Loading));
        app.handle_worker_event(Event::Progress { sent: 5, total: 10 });
        assert_eq!(app.progress, Some((5, 10)));
        app.handle_worker_event(Event::Status(Status::Booted(BootOutcome::Confirmed)));
        assert_eq!(app.progress, None);
        app.handle_worker_event(Event::Console(b"hi\n".to_vec()));
        assert_eq!(app.console.lines()[0], b"hi");
        app.handle_worker_event(Event::Fatal("port closed".into()));
        assert!(!app.console_active());
        assert!(app.log.back().unwrap().error);
    }
}
