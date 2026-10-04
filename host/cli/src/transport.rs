use anyhow::Context;
use clap::ValueEnum;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;
use tokio_serial::SerialPortBuilderExt;

pub trait Stream: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Stream for T {}

pub type Transport = Box<dyn Stream>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum FlowControl {
    /// RTS/CTS
    Hardware,
    None,
}

/// Opens a serial port, or a TCP connection when `port` is `tcp:HOST:PORT`
/// (for example QEMU started with `-serial tcp:127.0.0.1:4444,server=on`)
pub async fn open(port: &str, baud: u32, flow_control: FlowControl) -> anyhow::Result<Transport> {
    if let Some(addr) = port.strip_prefix("tcp:") {
        let stream = TcpStream::connect(addr)
            .await
            .with_context(|| format!("failed to connect to {addr}"))?;
        stream.set_nodelay(true)?;
        return Ok(Box::new(stream));
    }

    let flow_control = match flow_control {
        FlowControl::Hardware => tokio_serial::FlowControl::Hardware,
        FlowControl::None => tokio_serial::FlowControl::None,
    };
    let serial = tokio_serial::new(port, baud)
        .flow_control(flow_control)
        .open_native_async()
        .with_context(|| format!("failed to open {port}"))?;
    Ok(Box::new(serial))
}
