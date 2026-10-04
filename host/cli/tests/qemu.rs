//! End-to-end tests against the real chainloader running in QEMU's raspi4b
//! machine. They need qemu-system-aarch64 and zig, so they are ignored by
//! default. Run them with:
//!
//!     cargo test -p pichainboot --test qemu -- --ignored

use std::io::Write as _;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::{Child, Command, Stdio};
use std::sync::OnceLock;
use std::task::{Context, Poll};
use std::time::Duration;

use pichainboot_proto::message::{ErrorCode, MsgType, Request};
use pichainboot_proto::{BootOutcome, Client, Config, Error};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::net::TcpStream;
use tokio::time::timeout;

const LOAD_ADDR: u64 = 0x80000;
/// Where QEMU places the DTB
const DTB_ADDR: u64 = 0x800_0000;
const TEST_TIMEOUT: Duration = Duration::from_secs(60);

struct Images {
    loader: PathBuf,
    testkernel: PathBuf,
    dtb: PathBuf,
}

fn images() -> &'static Images {
    static IMAGES: OnceLock<Images> = OnceLock::new();
    IMAGES.get_or_init(|| {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        for dir in ["chainboot", "testkernel"] {
            let status = Command::new("zig")
                .arg("build")
                .current_dir(root.join(dir))
                .status()
                .expect("failed to run zig");
            assert!(status.success(), "zig build failed in {dir}");
        }
        Images {
            loader: root.join("chainboot/zig-out/bin/kernel8.img"),
            testkernel: root.join("testkernel/zig-out/bin/testkernel.img"),
            dtb: Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/min.dtb"),
        }
    })
}

fn testkernel() -> Vec<u8> {
    std::fs::read(&images().testkernel).unwrap()
}

/// The test kernel padded with zeros, so that it takes many chunks to send
fn padded_testkernel(len: usize) -> Vec<u8> {
    let mut image = testkernel();
    image.resize(len, 0);
    image
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

struct Qemu {
    child: Child,
    serial_port: u16,
    monitor: std::net::TcpStream,
}

impl Qemu {
    async fn start() -> (Self, TcpStream) {
        let images = images();
        let serial_port = free_port();
        let monitor_port = free_port();
        let child = Command::new("qemu-system-aarch64")
            .args(["-M", "raspi4b", "-display", "none"])
            .arg("-kernel")
            .arg(&images.loader)
            .arg("-dtb")
            .arg(&images.dtb)
            .arg("-serial")
            .arg(format!("tcp:127.0.0.1:{serial_port},server=on,wait=off"))
            .arg("-monitor")
            .arg(format!("tcp:127.0.0.1:{monitor_port},server=on,wait=off"))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("failed to start qemu-system-aarch64");

        let mut qemu = None;
        let mut child = Some(child);
        for _ in 0..100 {
            if let Ok(monitor) = std::net::TcpStream::connect(("127.0.0.1", monitor_port)) {
                qemu = Some(Self { child: child.take().unwrap(), serial_port, monitor });
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let qemu = qemu.expect("QEMU monitor did not come up");
        let serial = qemu.connect_serial().await;
        (qemu, serial)
    }

    async fn connect_serial(&self) -> TcpStream {
        for _ in 0..100 {
            if let Ok(stream) = TcpStream::connect(("127.0.0.1", self.serial_port)).await {
                stream.set_nodelay(true).unwrap();
                return stream;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("QEMU serial port did not come up");
    }

    /// Resets the machine, like pressing the reset button
    fn reset(&mut self) {
        self.monitor.write_all(b"system_reset\n").unwrap();
        self.monitor.flush().unwrap();
    }
}

impl Drop for Qemu {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn config() -> Config {
    Config {
        response_timeout: Duration::from_millis(500),
        ..Config::default()
    }
}

/// Reads until `needle` has been seen `count` times, returns everything read
async fn read_until(io: &mut (impl AsyncRead + Unpin), mut output: Vec<u8>, needle: &str, count: usize) -> String {
    let found = |output: &[u8]| String::from_utf8_lossy(output).matches(needle).count() >= count;
    while !found(&output) {
        let mut buf = [0; 1024];
        let n = io.read(&mut buf).await.unwrap();
        assert!(n > 0, "connection closed, output so far: {}", String::from_utf8_lossy(&output));
        output.extend_from_slice(&buf[..n]);
    }
    String::from_utf8_lossy(&output).into_owned()
}

fn booted_line() -> String {
    format!("testkernel: booted, dtb=0x{DTB_ADDR:016x}")
}

#[tokio::test]
#[ignore = "needs qemu-system-aarch64 and zig"]
async fn boots_the_test_kernel() {
    timeout(TEST_TIMEOUT, async {
        let (_qemu, serial) = Qemu::start().await;
        let mut client = Client::new(serial, config());
        let info = client.connect().await.unwrap();
        assert_eq!(info.version, 1);
        assert_eq!(info.max_payload, 4096);

        let outcome = client
            .load_and_boot(LOAD_ADDR, LOAD_ADDR, &testkernel(), |_, _| {})
            .await
            .unwrap();
        assert_eq!(outcome, BootOutcome::Confirmed);

        let (mut io, output) = client.into_parts();
        read_until(&mut io, output, &booted_line(), 1).await;
        io.write_all(b"echo me").await.unwrap();
        read_until(&mut io, Vec::new(), "echo me", 1).await;
    })
    .await
    .unwrap();
}

#[tokio::test]
#[ignore = "needs qemu-system-aarch64 and zig"]
async fn rejects_loading_over_the_loader_and_the_dtb() {
    timeout(TEST_TIMEOUT, async {
        let (_qemu, serial) = Qemu::start().await;
        let mut client = Client::new(serial, config());
        client.connect().await.unwrap();

        for addr in [0x7_0000, 0x200_0000, DTB_ADDR] {
            match client.request(&Request::MemWrite { addr, size: 16 }).await {
                Err(Error::Device { req: MsgType::MemWrite, code: ErrorCode::BadAddress }) => {}
                other => panic!("unexpected result for {addr:#x}: {other:?}"),
            }
        }
        client.request(&Request::MemWrite { addr: LOAD_ADDR, size: 16 }).await.unwrap();
    })
    .await
    .unwrap();
}

/// Corrupts and drops bytes written by the host. Faults are rare enough that
/// a resent frame normally gets through: a full DATA frame is 4105 bytes.
struct NoisyStream<T> {
    inner: T,
    written: u64,
    faults: u32,
}

impl<T> NoisyStream<T> {
    fn flip(pos: u64) -> bool {
        pos % 12007 == 12006
    }

    fn drop(pos: u64) -> bool {
        pos % 17011 == 17010
    }
}

impl<T: AsyncRead + Unpin> AsyncRead for NoisyStream<T> {
    fn poll_read(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl<T: AsyncWrite + Unpin> AsyncWrite for NoisyStream<T> {
    fn poll_write(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<std::io::Result<usize>> {
        let pos = self.written;
        if Self::drop(pos) {
            self.written += 1;
            self.faults += 1;
            return Poll::Ready(Ok(1));
        }

        let result = if Self::flip(pos) {
            Pin::new(&mut self.inner).poll_write(cx, &[buf[0] ^ 0xFF])
        } else {
            // Write up to the next byte that needs to be tampered with
            let len = (1..buf.len())
                .find(|&i| Self::flip(pos + i as u64) || Self::drop(pos + i as u64))
                .unwrap_or(buf.len());
            Pin::new(&mut self.inner).poll_write(cx, &buf[..len])
        };
        if let Poll::Ready(Ok(n)) = result {
            if Self::flip(pos) {
                self.faults += 1;
            }
            self.written += n as u64;
        }
        result
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

#[tokio::test]
#[ignore = "needs qemu-system-aarch64 and zig"]
async fn survives_a_noisy_link() {
    timeout(TEST_TIMEOUT, async {
        let (_qemu, serial) = Qemu::start().await;
        let noisy = NoisyStream { inner: serial, written: 0, faults: 0 };
        let mut client = Client::new(noisy, config());
        client.connect().await.unwrap();

        let outcome = client
            .load_and_boot(LOAD_ADDR, LOAD_ADDR, &padded_testkernel(64 * 1024), |_, _| {})
            .await
            .unwrap();
        assert_eq!(outcome, BootOutcome::Confirmed);
        let (mut io, output) = client.into_parts();
        assert!(io.faults >= 5, "only {} faults were injected", io.faults);
        read_until(&mut io, output, &booted_line(), 1).await;
    })
    .await
    .unwrap();
}

#[tokio::test]
#[ignore = "needs qemu-system-aarch64 and zig"]
async fn starts_over_when_the_device_resets_during_a_load() {
    timeout(TEST_TIMEOUT, async {
        let (mut qemu, serial) = Qemu::start().await;
        let mut client = Client::new(serial, config());
        client.connect().await.unwrap();

        let image = padded_testkernel(64 * 1024);
        let mut reset_done = false;
        let mut restarted = false;
        let outcome = client
            .load_and_boot(LOAD_ADDR, LOAD_ADDR, &image, |sent, total| {
                if !reset_done && sent > total / 2 {
                    reset_done = true;
                    qemu.reset();
                }
                if reset_done && sent < total / 2 {
                    restarted = true;
                }
            })
            .await
            .unwrap();
        assert_eq!(outcome, BootOutcome::Confirmed);
        assert!(restarted, "the load did not start over");

        let (mut io, output) = client.into_parts();
        read_until(&mut io, output, &booted_line(), 1).await;
    })
    .await
    .unwrap();
}

#[tokio::test]
#[ignore = "needs qemu-system-aarch64 and zig"]
async fn headless_auto_mode_boots_again_after_a_reset() {
    timeout(TEST_TIMEOUT, async {
        let (mut qemu, serial) = Qemu::start().await;
        // The binary opens its own connection
        drop(serial);

        let mut host = tokio::process::Command::new(env!("CARGO_BIN_EXE_pichainboot"))
            .args(["--headless", "--auto", "--port"])
            .arg(format!("tcp:127.0.0.1:{}", qemu.serial_port))
            .arg(&images().testkernel)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let mut stdout = host.stdout.take().unwrap();

        let output = read_until(&mut stdout, Vec::new(), &booted_line(), 1).await;
        qemu.reset();
        read_until(&mut stdout, output.into_bytes(), &booted_line(), 2).await;
    })
    .await
    .unwrap();
}
