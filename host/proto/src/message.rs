//! Requests, responses and their payloads, see docs/protocol.md.

use std::fmt;

use crate::frame::encode_frame;

pub const PROTOCOL_VERSION: u16 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum MsgType {
    Ping = 0x01,
    Info = 0x02,
    MemWrite = 0x10,
    Data = 0x11,
    End = 0x12,
    Boot = 0x13,
    Ok = 0x80,
    Err = 0xFF,
}

impl MsgType {
    pub fn from_u8(value: u8) -> Option<Self> {
        Some(match value {
            0x01 => Self::Ping,
            0x02 => Self::Info,
            0x10 => Self::MemWrite,
            0x11 => Self::Data,
            0x12 => Self::End,
            0x13 => Self::Boot,
            0x80 => Self::Ok,
            0xFF => Self::Err,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCode {
    BadFrame,
    Unsupported,
    BadPayload,
    BadAddress,
    NoTransfer,
    OutOfRange,
    Checksum,
    NotVerified,
    Unknown(u8),
}

impl ErrorCode {
    pub fn from_u8(value: u8) -> Self {
        match value {
            0x01 => Self::BadFrame,
            0x02 => Self::Unsupported,
            0x03 => Self::BadPayload,
            0x04 => Self::BadAddress,
            0x05 => Self::NoTransfer,
            0x06 => Self::OutOfRange,
            0x07 => Self::Checksum,
            0x08 => Self::NotVerified,
            other => Self::Unknown(other),
        }
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadFrame => write!(f, "frame was corrupted"),
            Self::Unsupported => write!(f, "request not supported"),
            Self::BadPayload => write!(f, "invalid payload"),
            Self::BadAddress => write!(f, "memory range not allowed"),
            Self::NoTransfer => write!(f, "no active transfer"),
            Self::OutOfRange => write!(f, "data past the end of the transfer"),
            Self::Checksum => write!(f, "checksum mismatch"),
            Self::NotVerified => write!(f, "entry point is not in a verified transfer"),
            Self::Unknown(code) => write!(f, "unknown error 0x{code:02x}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request<'a> {
    Ping,
    Info,
    MemWrite { addr: u64, size: u32 },
    Data { offset: u32, bytes: &'a [u8] },
    End { crc: u32 },
    Boot { entry: u64 },
}

impl Request<'_> {
    pub fn msg_type(&self) -> MsgType {
        match self {
            Self::Ping => MsgType::Ping,
            Self::Info => MsgType::Info,
            Self::MemWrite { .. } => MsgType::MemWrite,
            Self::Data { .. } => MsgType::Data,
            Self::End { .. } => MsgType::End,
            Self::Boot { .. } => MsgType::Boot,
        }
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut payload = Vec::new();
        match self {
            Self::Ping | Self::Info => {}
            Self::MemWrite { addr, size } => {
                payload.extend_from_slice(&addr.to_le_bytes());
                payload.extend_from_slice(&size.to_le_bytes());
            }
            Self::Data { offset, bytes } => {
                payload.extend_from_slice(&offset.to_le_bytes());
                payload.extend_from_slice(bytes);
            }
            Self::End { crc } => payload.extend_from_slice(&crc.to_le_bytes()),
            Self::Boot { entry } => payload.extend_from_slice(&entry.to_le_bytes()),
        }
        encode_frame(self.msg_type() as u8, &payload)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Response {
    Ok { req: u8, data: Vec<u8> },
    Err { req: u8, code: ErrorCode },
}

impl Response {
    /// Parses a frame sent by the device, returns None if it is not a valid response
    pub fn parse(msg_type: u8, payload: &[u8]) -> Option<Self> {
        match (MsgType::from_u8(msg_type)?, payload) {
            (MsgType::Ok, [req, data @ ..]) => Some(Self::Ok { req: *req, data: data.to_vec() }),
            (MsgType::Err, [req, code]) => Some(Self::Err { req: *req, code: ErrorCode::from_u8(*code) }),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capabilities(pub u32);

impl Capabilities {
    pub fn mem_boot(self) -> bool {
        self.0 & 1 != 0
    }

    pub fn sd_files(self) -> bool {
        self.0 & 2 != 0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Info {
    pub version: u16,
    pub max_payload: u16,
    pub capabilities: Capabilities,
}

impl Info {
    pub fn parse(data: &[u8]) -> Option<Self> {
        let data: &[u8; 8] = data.get(..8)?.try_into().ok()?;
        Some(Self {
            version: u16::from_le_bytes([data[0], data[1]]),
            max_payload: u16::from_le_bytes([data[2], data[3]]),
            capabilities: Capabilities(u32::from_le_bytes([data[4], data[5], data[6], data[7]])),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_request_payloads() {
        let frame = Request::MemWrite { addr: 0x80000, size: 0x1234 }.encode();
        assert_eq!(frame[2], 0x10);
        assert_eq!(&frame[5..17], &[0, 0, 8, 0, 0, 0, 0, 0, 0x34, 0x12, 0, 0]);

        let frame = Request::Data { offset: 4, bytes: b"ab" }.encode();
        assert_eq!(&frame[5..11], &[4, 0, 0, 0, b'a', b'b']);
    }

    #[test]
    fn parses_responses() {
        assert_eq!(
            Response::parse(0x80, &[0x10]),
            Some(Response::Ok { req: 0x10, data: vec![] })
        );
        assert_eq!(
            Response::parse(0xFF, &[0x13, 0x08]),
            Some(Response::Err { req: 0x13, code: ErrorCode::NotVerified })
        );
        assert_eq!(Response::parse(0x80, &[]), None);
        assert_eq!(Response::parse(0x01, &[0x10]), None);
    }

    #[test]
    fn parses_info() {
        let info = Info::parse(&[0x01, 0x00, 0x00, 0x10, 0x01, 0x00, 0x00, 0x00]).unwrap();
        assert_eq!(info.version, 1);
        assert_eq!(info.max_payload, 4096);
        assert!(info.capabilities.mem_boot());
        assert!(!info.capabilities.sd_files());
        assert_eq!(Info::parse(&[0x01]), None);
    }
}
