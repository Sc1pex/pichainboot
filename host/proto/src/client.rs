//! Host side of the protocol, over any async byte stream.

use std::collections::VecDeque;
use std::io;
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::time::{Instant, timeout_at};
use tracing::{debug, warn};

use crate::frame::{Decoder, Event, MAGIC};
use crate::message::{ErrorCode, Info, MsgType, PROTOCOL_VERSION, Request, Response};

/// How many times a load is started over when the device restarts the session
const MAX_SESSION_RESTARTS: u32 = 3;
/// How many times the whole image is resent when END reports a checksum mismatch
const MAX_IMAGE_PASSES: u32 = 3;
/// BOOT is only resent once. If the device already jumped, the frame ends up as
/// console input of the new kernel.
const BOOT_RETRIES: u32 = 1;

#[derive(Debug, Clone)]
pub struct Config {
    /// How long to wait for a response before resending a request
    pub response_timeout: Duration,
    /// How many times a request is resent before giving up
    pub max_retries: u32,
    /// How long to listen for the device announcing itself before sending INFO
    pub listen_window: Duration,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            response_timeout: Duration::from_secs(1),
            max_retries: 5,
            listen_window: Duration::from_millis(300),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("i/o error: {0}")]
    Io(#[from] io::Error),
    #[error("connection closed")]
    Closed,
    #[error("no response to {0:?} after {1} attempts")]
    Timeout(MsgType, u32),
    #[error("device rejected {req:?}: {code}")]
    Device { req: MsgType, code: ErrorCode },
    #[error("device sent an invalid INFO response")]
    BadInfo,
    #[error("device speaks protocol version {0}, expected {PROTOCOL_VERSION}")]
    UnsupportedVersion(u16),
    #[error("device does not support loading images into memory")]
    NoMemBoot,
    #[error("not connected to the device")]
    NotConnected,
    #[error("the device restarted the session")]
    SessionRestarted,
    #[error("image is empty")]
    EmptyImage,
    #[error("image is too large ({0} bytes)")]
    ImageTooLarge(usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootOutcome {
    /// The device acknowledged BOOT before jumping
    Confirmed,
    /// No acknowledgement arrived, the device may or may not have jumped
    Unconfirmed,
}

pub struct Client<T> {
    io: T,
    config: Config,
    decoder: Decoder,
    /// Bytes read from `io` that have not been decoded yet
    pending: VecDeque<u8>,
    info: Option<Info>,
}

impl<T: AsyncRead + AsyncWrite + Unpin> Client<T> {
    pub fn new(io: T, config: Config) -> Self {
        Self::with_pending(io, config, Vec::new())
    }

    /// Creates a client for a stream from which `pending` was already read
    pub fn with_pending(io: T, config: Config, pending: Vec<u8>) -> Self {
        Self {
            io,
            config,
            decoder: Decoder::new(),
            pending: pending.into(),
            info: None,
        }
    }

    pub fn info(&self) -> Option<Info> {
        self.info
    }

    /// Returns the stream and the bytes read from it but not decoded yet.
    /// After a boot, those bytes are the first console output.
    pub fn into_parts(self) -> (T, Vec<u8>) {
        (self.io, self.pending.into())
    }

    /// Establishes a session, waiting for the device as long as needed
    pub async fn connect(&mut self) -> Result<Info, Error> {
        loop {
            // A device that is announcing itself is answered with the magic
            // sequence. Otherwise it may already be in a session, so ask for INFO.
            let deadline = Instant::now() + self.config.listen_window;
            let mut announced = false;
            while let Some(event) = self.next_event(deadline).await? {
                match event {
                    Event::Magic => {
                        announced = true;
                        break;
                    }
                    Event::Frame { msg_type, payload } => {
                        if let Some(info) = self.info_response(msg_type, &payload)? {
                            return Ok(info);
                        }
                    }
                    Event::BadCrc { .. } => {}
                }
            }

            if announced {
                debug!("device announced itself, echoing");
                self.send(MAGIC).await?;
            } else {
                self.send(&Request::Info.encode()).await?;
            }

            let deadline = Instant::now() + self.config.response_timeout;
            while let Some(event) = self.next_event(deadline).await? {
                if let Event::Frame { msg_type, payload } = event
                    && let Some(info) = self.info_response(msg_type, &payload)?
                {
                    return Ok(info);
                }
            }
            self.decoder.reset();
        }
    }

    pub async fn ping(&mut self) -> Result<(), Error> {
        self.request(&Request::Ping).await.map(drop)
    }

    /// Loads `image` at `addr` and boots it at `entry`. Starts over if the
    /// device restarts the session along the way.
    pub async fn load_and_boot(
        &mut self,
        addr: u64,
        entry: u64,
        image: &[u8],
        mut progress: impl FnMut(usize, usize),
    ) -> Result<BootOutcome, Error> {
        let mut restarts = 0;
        loop {
            let result = match self.load(addr, image, &mut progress).await {
                Ok(()) => self.boot(entry).await,
                Err(e) => Err(e),
            };
            match result {
                Err(Error::SessionRestarted) if restarts < MAX_SESSION_RESTARTS => {
                    restarts += 1;
                    warn!("device restarted the session, starting over");
                }
                other => return other,
            }
        }
    }

    /// Writes `image` to device memory at `addr` and verifies it
    pub async fn load(
        &mut self,
        addr: u64,
        image: &[u8],
        progress: &mut impl FnMut(usize, usize),
    ) -> Result<(), Error> {
        let info = self.info.ok_or(Error::NotConnected)?;
        if image.is_empty() {
            return Err(Error::EmptyImage);
        }
        let size = u32::try_from(image.len()).map_err(|_| Error::ImageTooLarge(image.len()))?;
        let chunk_len = usize::from(info.max_payload) - 4;
        let crc = crc32fast::hash(image);

        self.request(&Request::MemWrite { addr, size }).await?;

        let mut pass = 1;
        loop {
            progress(0, image.len());
            for (i, bytes) in image.chunks(chunk_len).enumerate() {
                let offset = i * chunk_len;
                self.request(&Request::Data { offset: offset as u32, bytes }).await?;
                progress(offset + bytes.len(), image.len());
            }

            match self.request(&Request::End { crc }).await {
                Err(Error::Device { code: ErrorCode::Checksum, .. }) if pass < MAX_IMAGE_PASSES => {
                    pass += 1;
                    warn!("checksum mismatch, resending the image");
                }
                other => return other.map(drop),
            }
        }
    }

    /// Asks the device to jump to `entry` in the last verified transfer
    pub async fn boot(&mut self, entry: u64) -> Result<BootOutcome, Error> {
        match self.request_with_retries(&Request::Boot { entry }, BOOT_RETRIES).await {
            Ok(_) => Ok(BootOutcome::Confirmed),
            Err(Error::Timeout(..)) => Ok(BootOutcome::Unconfirmed),
            Err(e) => Err(e),
        }
    }

    /// Sends a request and returns the data of its `OK` response
    pub async fn request(&mut self, request: &Request<'_>) -> Result<Vec<u8>, Error> {
        self.request_with_retries(request, self.config.max_retries).await
    }

    async fn request_with_retries(&mut self, request: &Request<'_>, retries: u32) -> Result<Vec<u8>, Error> {
        let msg_type = request.msg_type();
        let frame = request.encode();

        for attempt in 0..=retries {
            if attempt > 0 {
                debug!("resending {msg_type:?}, attempt {}", attempt + 1);
            }
            self.send(&frame).await?;

            let deadline = Instant::now() + self.config.response_timeout;
            loop {
                let Some(event) = self.next_event(deadline).await? else {
                    self.decoder.reset();
                    break;
                };

                match event {
                    Event::Magic => {
                        // The device may have reset. If it did, it answers the
                        // echo with OK(INFO), otherwise it ignores it.
                        debug!("device announced itself, echoing");
                        self.send(MAGIC).await?;
                    }
                    Event::BadCrc { .. } => {}
                    Event::Frame { msg_type: t, payload } => match Response::parse(t, &payload) {
                        Some(Response::Ok { req, data }) if req == msg_type as u8 => {
                            if msg_type == MsgType::Info {
                                self.store_info(&data)?;
                            }
                            return Ok(data);
                        }
                        Some(Response::Ok { req, data }) if req == MsgType::Info as u8 => {
                            self.store_info(&data)?;
                            return Err(Error::SessionRestarted);
                        }
                        Some(Response::Err { code: ErrorCode::BadFrame, .. }) => break,
                        Some(Response::Err { req, code }) if req == msg_type as u8 => {
                            return Err(Error::Device { req: msg_type, code });
                        }
                        // A late response to an earlier attempt
                        _ => {}
                    },
                }
            }
        }
        Err(Error::Timeout(msg_type, retries + 1))
    }

    fn info_response(&mut self, msg_type: u8, payload: &[u8]) -> Result<Option<Info>, Error> {
        match Response::parse(msg_type, payload) {
            Some(Response::Ok { req, data }) if req == MsgType::Info as u8 => self.store_info(&data).map(Some),
            _ => Ok(None),
        }
    }

    fn store_info(&mut self, data: &[u8]) -> Result<Info, Error> {
        let info = Info::parse(data).ok_or(Error::BadInfo)?;
        if info.version != PROTOCOL_VERSION {
            return Err(Error::UnsupportedVersion(info.version));
        }
        if !info.capabilities.mem_boot() {
            return Err(Error::NoMemBoot);
        }
        if info.max_payload <= 4 {
            return Err(Error::BadInfo);
        }
        self.info = Some(info);
        Ok(info)
    }

    async fn send(&mut self, bytes: &[u8]) -> Result<(), Error> {
        self.io.write_all(bytes).await?;
        self.io.flush().await?;
        Ok(())
    }

    /// Returns the next decoded event, or None once `deadline` passes
    async fn next_event(&mut self, deadline: Instant) -> Result<Option<Event>, Error> {
        loop {
            while let Some(byte) = self.pending.pop_front() {
                if let Some(event) = self.decoder.feed(byte) {
                    return Ok(Some(event));
                }
            }

            let mut buf = [0; 4096];
            match timeout_at(deadline, self.io.read(&mut buf)).await {
                Err(_) => return Ok(None),
                Ok(Ok(0)) => return Err(Error::Closed),
                Ok(Ok(n)) => self.pending.extend(&buf[..n]),
                Ok(Err(e)) => return Err(e.into()),
            }
        }
    }
}
