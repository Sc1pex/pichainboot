//! Helpers shared by the end-to-end tests: building the images and running
//! the chainloader in QEMU's raspi4b machine.

#![allow(dead_code)]

use std::io::Write as _;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::OnceLock;
use std::time::Duration;

use pichainboot_proto::Config;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::net::TcpStream;

pub const LOAD_ADDR: u64 = 0x80000;
/// Where QEMU places the DTB
pub const DTB_ADDR: u64 = 0x800_0000;
pub const TEST_TIMEOUT: Duration = Duration::from_secs(60);

pub struct Images {
    pub loader: PathBuf,
    pub testkernel: PathBuf,
    pub dtb: PathBuf,
}

pub fn images() -> &'static Images {
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

pub fn testkernel() -> Vec<u8> {
    std::fs::read(&images().testkernel).unwrap()
}

/// The test kernel padded with zeros, so that it takes many chunks to send
pub fn padded_testkernel(len: usize) -> Vec<u8> {
    let mut image = testkernel();
    image.resize(len, 0);
    image
}

pub fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

pub struct Qemu {
    child: Child,
    pub serial_port: u16,
    monitor: std::net::TcpStream,
}

impl Qemu {
    pub async fn start() -> (Self, TcpStream) {
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

    pub async fn connect_serial(&self) -> TcpStream {
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
    pub fn reset(&mut self) {
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

pub fn config() -> Config {
    Config {
        response_timeout: Duration::from_millis(500),
        ..Config::default()
    }
}

/// Reads until `needle` has been seen `count` times, returns everything read
pub async fn read_until(io: &mut (impl AsyncRead + Unpin), mut output: Vec<u8>, needle: &str, count: usize) -> String {
    let found = |output: &[u8]| String::from_utf8_lossy(output).matches(needle).count() >= count;
    while !found(&output) {
        let mut buf = [0; 1024];
        let n = io.read(&mut buf).await.unwrap();
        assert!(n > 0, "connection closed, output so far: {}", String::from_utf8_lossy(&output));
        output.extend_from_slice(&buf[..n]);
    }
    String::from_utf8_lossy(&output).into_owned()
}

pub fn booted_line() -> String {
    format!("testkernel: booted, dtb=0x{DTB_ADDR:016x}")
}
