//! Tests the client against a simplified fake device over an in-memory stream.
//! The real device is tested end to end in QEMU, see cli/tests/qemu.rs.

use std::collections::HashSet;
use std::time::Duration;

use pichainboot_proto::frame::{Decoder, Event, MAGIC, MagicMatcher, encode_frame};
use pichainboot_proto::message::{ErrorCode, MsgType};
use pichainboot_proto::{BootOutcome, Client, Config, Error};
use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream, duplex};

const MEM_BASE: u64 = 0x80000;
const MAX_PAYLOAD: u16 = 64;

#[derive(Default, Clone)]
struct Faults {
    /// Start in a session instead of announcing
    in_session: bool,
    /// Indexes of requests whose response is not sent
    drop_responses: HashSet<usize>,
    /// Indexes of requests that are answered with ERR BAD_FRAME without being handled
    corrupt_requests: HashSet<usize>,
    /// Index of the request at which the device resets and starts a new session
    restart_at: Option<usize>,
    /// Indexes of DATA requests whose bytes are stored corrupted
    corrupt_data: HashSet<usize>,
    /// Fail MEM_WRITE with this error
    reject_mem_write: Option<ErrorCode>,
}

struct Outcome {
    mem: Vec<u8>,
    booted: Option<u64>,
    requests: usize,
}

fn ok(req: MsgType, data: &[u8]) -> Vec<u8> {
    let mut payload = vec![req as u8];
    payload.extend_from_slice(data);
    encode_frame(MsgType::Ok as u8, &payload)
}

fn err(req: u8, code: u8) -> Vec<u8> {
    encode_frame(MsgType::Err as u8, &[req, code])
}

fn info() -> Vec<u8> {
    let mut data = 1u16.to_le_bytes().to_vec();
    data.extend_from_slice(&MAX_PAYLOAD.to_le_bytes());
    data.extend_from_slice(&1u32.to_le_bytes());
    ok(MsgType::Info, &data)
}

async fn fake_device(mut io: DuplexStream, faults: Faults) -> Outcome {
    let mut in_session = faults.in_session;
    let mut magic = MagicMatcher::default();
    let mut decoder = Decoder::new();
    let mut mem = vec![0u8; 0x1000];
    let mut transfer: Option<(u64, u32)> = None;
    let mut verified = false;
    let mut requests = 0;
    let mut booted = None;
    let mut announce = tokio::time::interval(Duration::from_millis(20));
    let mut buf = [0u8; 1024];

    loop {
        let n = tokio::select! {
            _ = announce.tick(), if !in_session => {
                io.write_all(MAGIC).await.unwrap();
                continue;
            }
            n = io.read(&mut buf) => n.unwrap(),
        };
        if n == 0 {
            return Outcome { mem, booted, requests };
        }

        for &byte in &buf[..n] {
            if booted.is_some() {
                // The "kernel" ignores its input
                continue;
            }
            if !in_session {
                if magic.feed(byte) {
                    in_session = true;
                    io.write_all(&info()).await.unwrap();
                }
                continue;
            }

            let Some(Event::Frame { msg_type, payload }) = decoder.feed(byte) else {
                continue;
            };
            let index = requests;
            requests += 1;

            if faults.restart_at == Some(index) {
                transfer = None;
                verified = false;
                io.write_all(&info()).await.unwrap();
                continue;
            }
            if faults.corrupt_requests.contains(&index) {
                io.write_all(&err(msg_type, 0x01)).await.unwrap();
                continue;
            }

            let response = match MsgType::from_u8(msg_type) {
                Some(MsgType::Ping) => ok(MsgType::Ping, &[]),
                Some(MsgType::Info) => {
                    transfer = None;
                    verified = false;
                    info()
                }
                Some(MsgType::MemWrite) => {
                    if let Some(code) = faults.reject_mem_write {
                        err(msg_type, error_code(code))
                    } else {
                        let addr = u64::from_le_bytes(payload[0..8].try_into().unwrap());
                        let size = u32::from_le_bytes(payload[8..12].try_into().unwrap());
                        transfer = Some((addr, size));
                        verified = false;
                        ok(MsgType::MemWrite, &[])
                    }
                }
                Some(MsgType::Data) => {
                    let (addr, _) = transfer.unwrap();
                    let offset = u32::from_le_bytes(payload[0..4].try_into().unwrap()) as usize;
                    let start = (addr - MEM_BASE) as usize + offset;
                    mem[start..start + payload.len() - 4].copy_from_slice(&payload[4..]);
                    if faults.corrupt_data.contains(&index) {
                        mem[start] ^= 0xFF;
                    }
                    verified = false;
                    ok(MsgType::Data, &[])
                }
                Some(MsgType::End) => {
                    let (addr, size) = transfer.unwrap();
                    let start = (addr - MEM_BASE) as usize;
                    let expected = u32::from_le_bytes(payload[0..4].try_into().unwrap());
                    if crc32fast::hash(&mem[start..start + size as usize]) == expected {
                        verified = true;
                        ok(MsgType::End, &[])
                    } else {
                        err(msg_type, 0x07)
                    }
                }
                Some(MsgType::Boot) => {
                    let entry = u64::from_le_bytes(payload[0..8].try_into().unwrap());
                    if verified {
                        booted = Some(entry);
                        ok(MsgType::Boot, &[])
                    } else {
                        err(msg_type, 0x08)
                    }
                }
                _ => err(msg_type, 0x02),
            };

            if !faults.drop_responses.contains(&index) {
                io.write_all(&response).await.unwrap();
            }
            if booted.is_some() {
                io.write_all(b"kernel says hi").await.unwrap();
            }
        }
    }
}

fn error_code(code: ErrorCode) -> u8 {
    match code {
        ErrorCode::BadAddress => 0x04,
        _ => unimplemented!(),
    }
}

fn config() -> Config {
    Config {
        response_timeout: Duration::from_millis(100),
        max_retries: 3,
        listen_window: Duration::from_millis(50),
    }
}

fn start(faults: Faults) -> (Client<DuplexStream>, tokio::task::JoinHandle<Outcome>) {
    let (host, device) = duplex(64 * 1024);
    let device = tokio::spawn(fake_device(device, faults));
    (Client::new(host, config()), device)
}

fn image(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i * 7 + 3) as u8).collect()
}

/// Requests in a load of `image(200)`: MEM_WRITE, 4 DATA chunks, END, BOOT
const IMAGE_LEN: usize = 200;

async fn load_and_boot(client: &mut Client<DuplexStream>) -> Result<BootOutcome, Error> {
    client.load_and_boot(MEM_BASE, MEM_BASE, &image(IMAGE_LEN), |_, _| {}).await
}

#[tokio::test]
async fn connects_to_an_announcing_device() {
    let (mut client, _device) = start(Faults::default());
    let info = client.connect().await.unwrap();
    assert_eq!(info.version, 1);
    assert_eq!(info.max_payload, MAX_PAYLOAD);
    client.ping().await.unwrap();
}

#[tokio::test]
async fn connects_to_a_device_already_in_a_session() {
    let (mut client, _device) = start(Faults { in_session: true, ..Default::default() });
    client.connect().await.unwrap();
    client.ping().await.unwrap();
}

#[tokio::test]
async fn loads_and_boots_an_image() {
    let (mut client, device) = start(Faults::default());
    client.connect().await.unwrap();

    let mut last_progress = (0, 0);
    let outcome = client
        .load_and_boot(MEM_BASE, MEM_BASE + 4, &image(IMAGE_LEN), |sent, total| last_progress = (sent, total))
        .await
        .unwrap();
    assert_eq!(outcome, BootOutcome::Confirmed);
    assert_eq!(last_progress, (IMAGE_LEN, IMAGE_LEN));

    // Console output that followed OK(BOOT) is handed over
    let (mut io, mut console) = client.into_parts();
    while console.len() < b"kernel says hi".len() {
        let mut buf = [0; 64];
        let n = io.read(&mut buf).await.unwrap();
        console.extend_from_slice(&buf[..n]);
    }
    assert_eq!(console, b"kernel says hi");

    drop(io);
    let outcome = device.await.unwrap();
    assert_eq!(outcome.booted, Some(MEM_BASE + 4));
    assert_eq!(&outcome.mem[..IMAGE_LEN], image(IMAGE_LEN));
}

#[tokio::test]
async fn resends_requests_whose_response_was_lost() {
    let (mut client, device) = start(Faults {
        drop_responses: [0, 2, 5].into(),
        ..Default::default()
    });
    client.connect().await.unwrap();
    assert_eq!(load_and_boot(&mut client).await.unwrap(), BootOutcome::Confirmed);
    drop(client);
    let outcome = device.await.unwrap();
    assert_eq!(&outcome.mem[..IMAGE_LEN], image(IMAGE_LEN));
    assert_eq!(outcome.requests, 7 + 3);
}

#[tokio::test]
async fn resends_corrupted_requests() {
    let (mut client, device) = start(Faults {
        corrupt_requests: [1, 3].into(),
        ..Default::default()
    });
    client.connect().await.unwrap();
    assert_eq!(load_and_boot(&mut client).await.unwrap(), BootOutcome::Confirmed);
    drop(client);
    assert_eq!(&device.await.unwrap().mem[..IMAGE_LEN], image(IMAGE_LEN));
}

#[tokio::test]
async fn resends_the_image_after_a_checksum_mismatch() {
    let (mut client, device) = start(Faults {
        corrupt_data: [2].into(),
        ..Default::default()
    });
    client.connect().await.unwrap();
    assert_eq!(load_and_boot(&mut client).await.unwrap(), BootOutcome::Confirmed);
    drop(client);
    let outcome = device.await.unwrap();
    assert_eq!(&outcome.mem[..IMAGE_LEN], image(IMAGE_LEN));
    // MEM_WRITE, 4 DATA, failed END, 4 DATA, END, BOOT
    assert_eq!(outcome.requests, 12);
}

#[tokio::test]
async fn starts_over_when_the_session_restarts() {
    let (mut client, device) = start(Faults {
        restart_at: Some(3),
        ..Default::default()
    });
    client.connect().await.unwrap();
    assert_eq!(load_and_boot(&mut client).await.unwrap(), BootOutcome::Confirmed);
    drop(client);
    let outcome = device.await.unwrap();
    assert_eq!(&outcome.mem[..IMAGE_LEN], image(IMAGE_LEN));
    // 3 requests before the restart, the restart itself, then a full load
    assert_eq!(outcome.requests, 3 + 1 + 7);
}

#[tokio::test]
async fn reports_device_errors() {
    let (mut client, _device) = start(Faults {
        reject_mem_write: Some(ErrorCode::BadAddress),
        ..Default::default()
    });
    client.connect().await.unwrap();
    match load_and_boot(&mut client).await {
        Err(Error::Device { req: MsgType::MemWrite, code: ErrorCode::BadAddress }) => {}
        other => panic!("unexpected result {other:?}"),
    }
}

#[tokio::test]
async fn reports_an_unconfirmed_boot() {
    let (mut client, _device) = start(Faults {
        // Both BOOT attempts go unanswered
        drop_responses: [6, 7].into(),
        ..Default::default()
    });
    client.connect().await.unwrap();
    assert_eq!(load_and_boot(&mut client).await.unwrap(), BootOutcome::Unconfirmed);
}

#[tokio::test]
async fn times_out_when_the_device_stops_answering() {
    let (mut client, _device) = start(Faults {
        drop_responses: (1..100).collect(),
        ..Default::default()
    });
    client.connect().await.unwrap();
    match load_and_boot(&mut client).await {
        Err(Error::Timeout(MsgType::Data, 4)) => {}
        other => panic!("unexpected result {other:?}"),
    }
}

#[tokio::test]
async fn load_requires_a_connection() {
    let (mut client, _device) = start(Faults::default());
    assert!(matches!(load_and_boot(&mut client).await, Err(Error::NotConnected)));
}
