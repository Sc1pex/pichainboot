//! End-to-end test of the tcpuart support, with the tcpuart stack simulated:
//! a PTY bridged to the serial port of QEMU stands in for the PTY of the
//! daemon, and a fake daemon answers on its own socket, turning reset requests
//! into QEMU machine resets.
//!
//!     cargo test -p pichainboot --features tcpuart --test tcpuart -- --include-ignored

#![cfg(feature = "tcpuart")]

mod common;

use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::OwnedFd;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use common::*;
use futures::{SinkExt, StreamExt};
use nix::pty::openpty;
use nix::sys::termios;
use tcpuart_common::ctl::{
    ConnectionInfo, DaemonRequest, DaemonRequestDecoder, DaemonResponse, DaemonResponseEncoder,
};
use tokio::io::AsyncReadExt;
use tokio::net::{TcpStream, UnixListener};
use tokio::time::timeout;
use tokio_util::codec::{FramedRead, FramedWrite};

struct FakeDaemon {
    socket: PathBuf,
    resets: Arc<AtomicUsize>,
}

impl Drop for FakeDaemon {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.socket);
    }
}

/// Serves the daemon control protocol for one connection named "pi"
fn start_daemon(test: &str, pty_path: String, qemu: Option<Arc<Mutex<Qemu>>>) -> FakeDaemon {
    // Unix socket paths are limited to ~100 bytes, so stay in /tmp
    let socket = PathBuf::from(format!("/tmp/pichainboot-{test}-{}.sock", std::process::id()));
    let _ = std::fs::remove_file(&socket);
    let listener = UnixListener::bind(&socket).unwrap();
    let resets = Arc::new(AtomicUsize::new(0));

    let reset_count = resets.clone();
    tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let (reader, writer) = stream.into_split();
            let Some(Ok(request)) = FramedRead::new(reader, DaemonRequestDecoder).next().await else {
                continue;
            };
            let response = match request {
                DaemonRequest::List => DaemonResponse::List(vec![ConnectionInfo {
                    name: "pi".into(),
                    addr: 0x0A00_0002,
                    port: 15113,
                    pts_path: pty_path.clone(),
                    connected: true,
                }]),
                DaemonRequest::Reset { name } if name == "pi" => {
                    reset_count.fetch_add(1, Ordering::SeqCst);
                    if let Some(qemu) = &qemu {
                        qemu.lock().unwrap().reset();
                    }
                    DaemonResponse::ResetOk
                }
                _ => DaemonResponse::Error("unexpected request".into()),
            };
            FramedWrite::new(writer, DaemonResponseEncoder).send(response).await.unwrap();
        }
    });

    FakeDaemon { socket, resets }
}

/// Creates a PTY whose master side is bridged to `serial`, like the PTY the
/// daemon creates for a connection. Returns the path of the slave side.
fn bridge_pty(serial: TcpStream) -> (String, OwnedFd) {
    let pty = openpty(None, None).unwrap();
    let mut tio = termios::tcgetattr(&pty.slave).unwrap();
    termios::cfmakeraw(&mut tio);
    termios::tcsetattr(&pty.slave, termios::SetArg::TCSANOW, &tio).unwrap();
    let path = nix::unistd::ttyname(&pty.slave).unwrap().display().to_string();

    let serial = serial.into_std().unwrap();
    serial.set_nonblocking(false).unwrap();
    let master = File::from(pty.master);

    let (mut from_pty, mut to_serial) = (master.try_clone().unwrap(), serial.try_clone().unwrap());
    std::thread::spawn(move || copy(&mut from_pty, &mut to_serial));
    let (mut from_serial, mut to_pty) = (serial, master);
    std::thread::spawn(move || copy(&mut from_serial, &mut to_pty));

    // Keep the slave open, the master reports errors while nothing has it open
    (path, pty.slave)
}

fn copy(from: &mut impl Read, to: &mut impl Write) {
    let mut buf = [0; 4096];
    while let Ok(n) = from.read(&mut buf) {
        if n == 0 || to.write_all(&buf[..n]).is_err() {
            return;
        }
    }
}

fn host_command(daemon: &FakeDaemon, port: &str) -> tokio::process::Command {
    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_pichainboot"));
    command
        .args(["--headless", "--port", port, "--tcpuart-socket"])
        .arg(&daemon.socket)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    command
}

#[tokio::test]
#[ignore = "needs qemu-system-aarch64 and zig"]
async fn resets_and_boots_through_tcpuart() {
    timeout(TEST_TIMEOUT, async {
        let (qemu, serial) = Qemu::start().await;
        let (pty_path, _slave) = bridge_pty(serial);
        let daemon = start_daemon("boot", pty_path.clone(), Some(Arc::new(Mutex::new(qemu))));

        let mut host = host_command(&daemon, "tcpuart:pi")
            .arg("--reset")
            .arg(&images().testkernel)
            .spawn()
            .unwrap();
        let mut stdout = host.stdout.take().unwrap();
        read_until(&mut stdout, Vec::new(), &booted_line(), 1).await;
        assert_eq!(daemon.resets.load(Ordering::SeqCst), 1);

        host.kill().await.unwrap();
        let mut status = String::new();
        host.stderr.take().unwrap().read_to_string(&mut status).await.unwrap();
        assert!(status.contains(&format!("tcpuart connection 'pi' on {pty_path}")), "{status}");
        assert!(status.contains("resetting the device"), "{status}");
    })
    .await
    .unwrap();
}

#[tokio::test]
#[ignore = "needs qemu-system-aarch64 and zig"]
async fn reopens_the_pty_after_the_host_was_killed() {
    timeout(TEST_TIMEOUT, async {
        let (qemu, serial) = Qemu::start().await;
        let (pty_path, _slave) = bridge_pty(serial);
        let daemon = start_daemon("reopen", pty_path, Some(Arc::new(Mutex::new(qemu))));

        // The daemon keeps the PTY open, so state left on it by a killed host
        // (like exclusive mode) persists
        for _ in 0..2 {
            let mut host = host_command(&daemon, "tcpuart:pi")
                .arg("--reset")
                .arg(&images().testkernel)
                .spawn()
                .unwrap();
            let mut stdout = host.stdout.take().unwrap();
            read_until(&mut stdout, Vec::new(), &booted_line(), 1).await;
            host.kill().await.unwrap();
        }
        assert_eq!(daemon.resets.load(Ordering::SeqCst), 2);
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn reports_an_unknown_tcpuart_connection() {
    let daemon = start_daemon("unknown", "/dev/null".into(), None);
    let output = host_command(&daemon, "tcpuart:missing")
        .arg("kernel8.img")
        .output()
        .await
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("no connection named 'missing'"), "{stderr}");
}
