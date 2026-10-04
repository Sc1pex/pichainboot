#[cfg(feature = "tcpuart")]
use std::path::Path;

use anyhow::Context;
use clap::ValueEnum;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;
use tokio_serial::SerialPortBuilderExt;

#[cfg(feature = "tcpuart")]
use crate::tcpuart;

pub trait Stream: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Stream for T {}

pub type Transport = Box<dyn Stream>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum FlowControl {
    /// RTS/CTS
    Hardware,
    None,
}

/// A way to reset the device remotely
#[derive(Debug, Clone)]
pub enum Reset {
    #[cfg(feature = "tcpuart")]
    Tcpuart(tcpuart::Connection),
}

impl Reset {
    pub async fn reset(&self) -> anyhow::Result<()> {
        match *self {
            #[cfg(feature = "tcpuart")]
            Self::Tcpuart(ref connection) => connection.reset().await,
        }
    }
}

pub struct Opened {
    pub stream: Transport,
    pub reset: Option<Reset>,
    /// Things worth telling the user about the connection
    pub notes: Vec<String>,
}

pub struct Options<'a> {
    pub port: &'a str,
    pub baud: u32,
    pub flow_control: FlowControl,
    #[cfg(feature = "tcpuart")]
    pub tcpuart_socket: &'a Path,
}

/// Opens the connection to the device. `port` is one of:
/// - a serial port, e.g. /dev/ttyUSB0
/// - `tcp:HOST:PORT` for a serial port bridged over TCP, e.g. QEMU started with
///   `-serial tcp:127.0.0.1:4444,server=on`
/// - `tcpuart:NAME` for a connection of a tcpuart daemon (with the `tcpuart` feature)
pub async fn open(options: &Options<'_>) -> anyhow::Result<Opened> {
    let port = options.port;

    if let Some(addr) = port.strip_prefix("tcp:") {
        let stream = TcpStream::connect(addr)
            .await
            .with_context(|| format!("failed to connect to {addr}"))?;
        stream.set_nodelay(true)?;
        return Ok(Opened {
            stream: Box::new(stream),
            reset: None,
            notes: Vec::new(),
        });
    }

    if let Some(name) = port.strip_prefix("tcpuart:") {
        #[cfg(feature = "tcpuart")]
        return open_tcpuart(options, name).await;
        #[cfg(not(feature = "tcpuart"))]
        anyhow::bail!("cannot open tcpuart connection '{name}': built without the tcpuart feature");
    }

    Ok(Opened {
        stream: Box::new(open_serial(port, options.baud, options.flow_control)?),
        reset: None,
        notes: Vec::new(),
    })
}

fn open_serial(port: &str, baud: u32, flow_control: FlowControl) -> anyhow::Result<tokio_serial::SerialStream> {
    let flow_control = match flow_control {
        FlowControl::Hardware => tokio_serial::FlowControl::Hardware,
        FlowControl::None => tokio_serial::FlowControl::None,
    };
    tokio_serial::new(port, baud)
        .flow_control(flow_control)
        .open_native_async()
        .with_context(|| format!("failed to open {port}"))
}

#[cfg(feature = "tcpuart")]
async fn open_tcpuart(options: &Options<'_>, name: &str) -> anyhow::Result<Opened> {
    let connection = tcpuart::Connection::new(options.tcpuart_socket.to_path_buf(), name.to_string());
    let pty = connection.pty().await?;

    let mut notes = vec![format!("tcpuart connection '{name}' on {}", pty.path)];
    if !pty.online {
        notes.push(format!(
            "the tcpuart daemon is not connected to the ESP32 for '{name}' yet"
        ));
    }

    // The daemon forwards the baud rate and flow control set on the PTY to the ESP32
    let mut serial = open_serial(&pty.path, options.baud, options.flow_control)?;
    // Ports are opened in exclusive mode, which is only cleared when the last
    // file descriptor is closed. The daemon keeps the PTY open, so if we exit
    // without clearing it (killed, or skipping destructors) nobody could open
    // the PTY again.
    serial
        .set_exclusive(false)
        .with_context(|| format!("failed to configure {}", pty.path))?;

    Ok(Opened {
        stream: Box::new(serial),
        reset: Some(Reset::Tcpuart(connection)),
        notes,
    })
}
