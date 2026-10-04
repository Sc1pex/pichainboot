//! Talks to a tcpuart daemon (https://github.com/Sc1pex/tcpuart), which bridges
//! the Pi's UART over Wi-Fi through an ESP32 and exposes it as a PTY.

use std::path::PathBuf;

use anyhow::{Context, anyhow, bail};
use futures::{SinkExt, StreamExt};
use tcpuart_common::ctl::{DaemonRequest, DaemonRequestEncoder, DaemonResponse, DaemonResponseDecoder};
use tokio::net::UnixStream;
use tokio_util::codec::{FramedRead, FramedWrite};

pub const DEFAULT_SOCKET: &str = "/tmp/tcpuart.sock";

/// A named connection managed by the daemon
#[derive(Debug, Clone)]
pub struct Connection {
    socket: PathBuf,
    name: String,
}

pub struct Pty {
    pub path: String,
    /// Whether the daemon is currently connected to the ESP32
    pub online: bool,
}

impl Connection {
    pub fn new(socket: PathBuf, name: String) -> Self {
        Self { socket, name }
    }

    async fn request(&self, request: DaemonRequest) -> anyhow::Result<DaemonResponse> {
        let stream = UnixStream::connect(&self.socket).await.with_context(|| {
            format!("failed to connect to the tcpuart daemon at {}", self.socket.display())
        })?;
        let (reader, writer) = stream.into_split();
        FramedWrite::new(writer, DaemonRequestEncoder).send(request).await?;
        FramedRead::new(reader, DaemonResponseDecoder)
            .next()
            .await
            .ok_or_else(|| anyhow!("the tcpuart daemon closed the connection"))?
            .context("invalid response from the tcpuart daemon")
    }

    /// Looks up the PTY the daemon exposes for this connection
    pub async fn pty(&self) -> anyhow::Result<Pty> {
        let connections = match self.request(DaemonRequest::List).await? {
            DaemonResponse::List(connections) => connections,
            DaemonResponse::Error(message) => bail!("tcpuart daemon: {message}"),
            _ => bail!("unexpected response from the tcpuart daemon"),
        };
        let connection = connections
            .into_iter()
            .find(|c| c.name == self.name)
            .ok_or_else(|| anyhow!("the tcpuart daemon has no connection named '{}'", self.name))?;
        Ok(Pty {
            path: connection.pts_path,
            online: connection.connected,
        })
    }

    /// Pulses the reset line of the device through the ESP32
    pub async fn reset(&self) -> anyhow::Result<()> {
        match self.request(DaemonRequest::Reset { name: self.name.clone() }).await? {
            DaemonResponse::ResetOk => Ok(()),
            DaemonResponse::Error(message) => bail!("tcpuart daemon: {message}"),
            _ => bail!("unexpected response from the tcpuart daemon"),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use tcpuart_common::ctl::{ConnectionInfo, DaemonRequestDecoder, DaemonResponseEncoder};
    use tokio::net::UnixListener;

    use super::*;

    /// Answers daemon requests like the real daemon would, records reset requests
    async fn fake_daemon(listener: UnixListener, resets: Arc<Mutex<Vec<String>>>) {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let (reader, writer) = stream.into_split();
            let mut requests = FramedRead::new(reader, DaemonRequestDecoder);
            let mut responses = FramedWrite::new(writer, DaemonResponseEncoder);
            let Some(Ok(request)) = requests.next().await else { continue };
            let response = match request {
                DaemonRequest::List => DaemonResponse::List(vec![
                    ConnectionInfo {
                        name: "pi".into(),
                        addr: 0x0A00_0002,
                        port: 15113,
                        pts_path: "/dev/pts/7".into(),
                        connected: true,
                    },
                    ConnectionInfo {
                        name: "sleepy".into(),
                        addr: 0x0A00_0003,
                        port: 15113,
                        pts_path: "/dev/pts/8".into(),
                        connected: false,
                    },
                ]),
                DaemonRequest::Reset { name } if name == "pi" => {
                    resets.lock().unwrap().push(name);
                    DaemonResponse::ResetOk
                }
                DaemonRequest::Reset { name } => {
                    DaemonResponse::Error(format!("Connection '{name}' does not support reset command"))
                }
                _ => DaemonResponse::Error("unexpected request".into()),
            };
            responses.send(response).await.unwrap();
        }
    }

    fn socket_path() -> PathBuf {
        // Unix socket paths are limited to ~100 bytes, so stay in /tmp
        PathBuf::from(format!("/tmp/pichainboot-test-{}.sock", std::process::id()))
    }

    #[tokio::test]
    async fn looks_up_ptys_and_resets() {
        let socket = socket_path();
        let _ = std::fs::remove_file(&socket);
        let resets = Arc::new(Mutex::new(Vec::new()));
        tokio::spawn(fake_daemon(UnixListener::bind(&socket).unwrap(), resets.clone()));

        let pi = Connection::new(socket.clone(), "pi".into());
        let pty = pi.pty().await.unwrap();
        assert_eq!(pty.path, "/dev/pts/7");
        assert!(pty.online);
        pi.reset().await.unwrap();
        assert_eq!(*resets.lock().unwrap(), ["pi"]);

        let sleepy = Connection::new(socket.clone(), "sleepy".into());
        assert!(!sleepy.pty().await.unwrap().online);
        let error = sleepy.reset().await.unwrap_err().to_string();
        assert!(error.contains("does not support reset"), "{error}");

        let missing = Connection::new(socket.clone(), "missing".into());
        let error = missing.pty().await.err().unwrap().to_string();
        assert!(error.contains("no connection named 'missing'"), "{error}");

        let _ = std::fs::remove_file(&socket);
        let error = pi.pty().await.err().unwrap().to_string();
        assert!(error.contains("failed to connect to the tcpuart daemon"), "{error}");
    }
}
