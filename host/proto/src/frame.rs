//! Frame encoding and decoding, see docs/protocol.md.

pub const MAGIC: &[u8; 8] = b"PICHBT\r\n";
pub const SYNC: [u8; 2] = [0xA5, 0x5A];
/// sync + type + len
pub const HEADER_LEN: usize = 5;
pub const CRC_LEN: usize = 4;

fn frame_crc(msg_type: u8, payload: &[u8]) -> u32 {
    let mut hasher = crc32fast::Hasher::new();
    hasher.update(&[msg_type]);
    hasher.update(&(payload.len() as u16).to_le_bytes());
    hasher.update(payload);
    hasher.finalize()
}

/// Encodes a frame. Panics if the payload does not fit in a u16 length.
pub fn encode_frame(msg_type: u8, payload: &[u8]) -> Vec<u8> {
    let len = u16::try_from(payload.len()).expect("frame payload too long");

    let mut frame = Vec::with_capacity(HEADER_LEN + payload.len() + CRC_LEN);
    frame.extend_from_slice(&SYNC);
    frame.push(msg_type);
    frame.extend_from_slice(&len.to_le_bytes());
    frame.extend_from_slice(payload);
    frame.extend_from_slice(&frame_crc(msg_type, payload).to_le_bytes());
    frame
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Frame { msg_type: u8, payload: Vec<u8> },
    /// A complete frame arrived but its CRC did not match
    BadCrc { msg_type: u8 },
    /// The magic sequence arrived between frames
    Magic,
}

/// Recognizes the magic sequence in a byte stream
#[derive(Debug, Default)]
pub struct MagicMatcher {
    matched: usize,
}

impl MagicMatcher {
    /// Returns true when `byte` completes the magic sequence
    pub fn feed(&mut self, byte: u8) -> bool {
        if byte == MAGIC[self.matched] {
            self.matched += 1;
        } else {
            // The first magic byte appears nowhere else in the sequence, so a
            // mismatch can only be the start of a new match
            self.matched = usize::from(byte == MAGIC[0]);
        }

        if self.matched == MAGIC.len() {
            self.matched = 0;
            return true;
        }
        false
    }

    pub fn reset(&mut self) {
        self.matched = 0;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Sync0,
    Sync1,
    Header,
    Payload,
    Crc,
}

/// Splits a byte stream into frames and magic sequences, one byte at a time
#[derive(Debug)]
pub struct Decoder {
    state: State,
    magic: MagicMatcher,
    header: [u8; 3],
    header_pos: usize,
    payload: Vec<u8>,
    payload_len: usize,
    crc: [u8; CRC_LEN],
    crc_pos: usize,
}

impl Default for Decoder {
    fn default() -> Self {
        Self::new()
    }
}

impl Decoder {
    pub fn new() -> Self {
        Self {
            state: State::Sync0,
            magic: MagicMatcher::default(),
            header: [0; 3],
            header_pos: 0,
            payload: Vec::new(),
            payload_len: 0,
            crc: [0; CRC_LEN],
            crc_pos: 0,
        }
    }

    /// Drops a partially received frame
    pub fn reset(&mut self) {
        self.state = State::Sync0;
        self.magic.reset();
    }

    pub fn feed(&mut self, byte: u8) -> Option<Event> {
        match self.state {
            State::Sync0 | State::Sync1 => {
                // Only look for the magic sequence between frames
                if self.magic.feed(byte) {
                    self.state = State::Sync0;
                    return Some(Event::Magic);
                }
                self.feed_sync(byte);
            }
            State::Header => {
                self.header[self.header_pos] = byte;
                self.header_pos += 1;
                if self.header_pos == self.header.len() {
                    self.payload_len = u16::from_le_bytes([self.header[1], self.header[2]]).into();
                    self.payload.clear();
                    self.crc_pos = 0;
                    self.state = if self.payload_len == 0 { State::Crc } else { State::Payload };
                }
            }
            State::Payload => {
                self.payload.push(byte);
                if self.payload.len() == self.payload_len {
                    self.state = State::Crc;
                }
            }
            State::Crc => {
                self.crc[self.crc_pos] = byte;
                self.crc_pos += 1;
                if self.crc_pos == CRC_LEN {
                    self.state = State::Sync0;
                    self.magic.reset();
                    let msg_type = self.header[0];
                    if u32::from_le_bytes(self.crc) != frame_crc(msg_type, &self.payload) {
                        return Some(Event::BadCrc { msg_type });
                    }
                    return Some(Event::Frame {
                        msg_type,
                        payload: std::mem::take(&mut self.payload),
                    });
                }
            }
        }
        None
    }

    fn feed_sync(&mut self, byte: u8) {
        match self.state {
            State::Sync0 if byte == SYNC[0] => self.state = State::Sync1,
            State::Sync1 if byte == SYNC[1] => {
                self.state = State::Header;
                self.header_pos = 0;
                self.magic.reset();
            }
            State::Sync1 if byte != SYNC[0] => self.state = State::Sync0,
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode_all(decoder: &mut Decoder, bytes: &[u8]) -> Vec<Event> {
        bytes.iter().filter_map(|&b| decoder.feed(b)).collect()
    }

    #[test]
    fn encodes_frames_matching_the_reference_crc() {
        // Same reference frames as the device tests in chainboot/src/protocol.zig
        assert_eq!(
            encode_frame(0x01, &[]),
            [0xa5, 0x5a, 0x01, 0x00, 0x00, 0x25, 0xb3, 0x83, 0xfe]
        );
        assert_eq!(
            encode_frame(0x80, &[0x02, 0x01, 0x00, 0x00, 0x10, 0x01, 0x00, 0x00, 0x00]),
            [
                0xa5, 0x5a, 0x80, 0x09, 0x00, 0x02, 0x01, 0x00, 0x00, 0x10, 0x01, 0x00, 0x00, 0x00,
                0xf5, 0x4f, 0x5f, 0x5a
            ]
        );
    }

    #[test]
    fn decodes_frames_and_magic_between_noise() {
        let mut stream = b"noise".to_vec();
        stream.extend_from_slice(MAGIC);
        stream.extend_from_slice(&encode_frame(0x80, b"\x01"));
        stream.extend_from_slice(&[0xA5, 0x00]);
        stream.extend_from_slice(&encode_frame(0x01, &[]));

        let mut decoder = Decoder::new();
        assert_eq!(
            decode_all(&mut decoder, &stream),
            [
                Event::Magic,
                Event::Frame { msg_type: 0x80, payload: vec![0x01] },
                Event::Frame { msg_type: 0x01, payload: vec![] },
            ]
        );
    }

    #[test]
    fn ignores_magic_inside_a_frame() {
        let mut decoder = Decoder::new();
        assert_eq!(
            decode_all(&mut decoder, &encode_frame(0x80, MAGIC)),
            [Event::Frame { msg_type: 0x80, payload: MAGIC.to_vec() }]
        );
    }

    #[test]
    fn reports_corrupted_frames() {
        let mut frame = encode_frame(0x80, b"abc");
        frame[6] ^= 0xFF;
        let mut decoder = Decoder::new();
        assert_eq!(decode_all(&mut decoder, &frame), [Event::BadCrc { msg_type: 0x80 }]);
    }

    #[test]
    fn reset_drops_a_partial_frame() {
        let frame = encode_frame(0x80, b"abc");
        let mut decoder = Decoder::new();
        assert!(decode_all(&mut decoder, &frame[..6]).is_empty());
        decoder.reset();
        assert_eq!(
            decode_all(&mut decoder, &frame),
            [Event::Frame { msg_type: 0x80, payload: b"abc".to_vec() }]
        );
    }

    #[test]
    fn matches_magic_after_a_partial_match() {
        let mut matcher = MagicMatcher::default();
        assert!(b"xPICPICHBT\r\n".iter().any(|&b| matcher.feed(b)));
        assert!(!b"PICHBT\rPICHB".iter().any(|&b| matcher.feed(b)));
    }
}
