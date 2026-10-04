//! Console scrollback. Turns the raw byte stream from the device into lines.
//! Escape sequences are dropped rather than emulated.

use std::collections::VecDeque;

const MAX_LINES: usize = 10_000;
const TAB_WIDTH: usize = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Escape {
    None,
    /// After ESC
    Start,
    /// Inside ESC [ ... (CSI), ends with a byte in 0x40..=0x7E
    Csi,
    /// Inside ESC ] ... (OSC), ends with BEL or ESC \
    Osc,
    OscEsc,
}

#[derive(Debug)]
pub struct Console {
    /// The last line is the one being written
    lines: VecDeque<Vec<u8>>,
    escape: Escape,
    /// A carriage return was received: the next character overwrites the line
    carriage_return: bool,
}

impl Default for Console {
    fn default() -> Self {
        Self {
            lines: VecDeque::from([Vec::new()]),
            escape: Escape::None,
            carriage_return: false,
        }
    }
}

impl Console {
    pub fn lines(&self) -> &VecDeque<Vec<u8>> {
        &self.lines
    }

    pub fn clear(&mut self) {
        *self = Self::default();
    }

    /// Ends a partially written line, so that new output starts on its own line
    pub fn finish_line(&mut self) {
        if !self.current().is_empty() {
            self.push_byte(b'\n');
        }
        self.escape = Escape::None;
        self.carriage_return = false;
    }

    pub fn push(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.push_byte(byte);
        }
    }

    fn push_byte(&mut self, byte: u8) {
        match self.escape {
            Escape::None => {}
            Escape::Start => {
                self.escape = match byte {
                    b'[' => Escape::Csi,
                    b']' => Escape::Osc,
                    _ => Escape::None,
                };
                return;
            }
            Escape::Csi => {
                if (0x40..=0x7E).contains(&byte) {
                    self.escape = Escape::None;
                }
                return;
            }
            Escape::Osc => {
                self.escape = match byte {
                    0x07 => Escape::None,
                    0x1B => Escape::OscEsc,
                    _ => Escape::Osc,
                };
                return;
            }
            Escape::OscEsc => {
                self.escape = if byte == b'\\' { Escape::None } else { Escape::Osc };
                return;
            }
        }

        match byte {
            0x1B => self.escape = Escape::Start,
            b'\n' => {
                self.carriage_return = false;
                self.lines.push_back(Vec::new());
                if self.lines.len() > MAX_LINES {
                    self.lines.pop_front();
                }
            }
            b'\r' => self.carriage_return = true,
            0x08 => {
                self.current().pop();
            }
            b'\t' => {
                let line = self.current_after_cr();
                let spaces = TAB_WIDTH - line.len() % TAB_WIDTH;
                line.extend(std::iter::repeat_n(b' ', spaces));
            }
            0x00..=0x1F | 0x7F => {}
            _ => self.current_after_cr().push(byte),
        }
    }

    fn current(&mut self) -> &mut Vec<u8> {
        self.lines.back_mut().expect("there is always a current line")
    }

    /// The current line, cleared first if a carriage return preceded
    fn current_after_cr(&mut self) -> &mut Vec<u8> {
        if std::mem::take(&mut self.carriage_return) {
            self.current().clear();
        }
        self.current()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(console: &Console) -> Vec<String> {
        console
            .lines()
            .iter()
            .map(|l| String::from_utf8_lossy(l).into_owned())
            .collect()
    }

    #[test]
    fn splits_lines() {
        let mut console = Console::default();
        console.push(b"hello\r\nwor");
        console.push(b"ld\n");
        assert_eq!(lines(&console), ["hello", "world", ""]);
    }

    #[test]
    fn carriage_return_overwrites_the_line() {
        let mut console = Console::default();
        console.push(b"progress 10%\rprogress 20%");
        assert_eq!(lines(&console), ["progress 20%"]);
    }

    #[test]
    fn handles_backspace_and_tabs() {
        let mut console = Console::default();
        console.push(b"ab\x08c\td");
        assert_eq!(lines(&console), ["ac  d"]);
    }

    #[test]
    fn drops_escape_sequences() {
        let mut console = Console::default();
        console.push(b"\x1b[1;31mred\x1b[0m \x1b]0;title\x07ok \x1b]0;t\x1b\\done\x07");
        assert_eq!(lines(&console), ["red ok done"]);
    }

    #[test]
    fn finishes_partial_lines_only() {
        let mut console = Console::default();
        console.push(b"partial");
        console.finish_line();
        console.finish_line();
        console.push(b"next\n");
        console.finish_line();
        assert_eq!(lines(&console), ["partial", "next", ""]);
    }

    #[test]
    fn keeps_a_bounded_scrollback() {
        let mut console = Console::default();
        for _ in 0..MAX_LINES + 10 {
            console.push(b"line\n");
        }
        assert_eq!(console.lines().len(), MAX_LINES);
    }
}
