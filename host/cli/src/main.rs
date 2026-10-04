mod headless;
mod transport;
mod tui;
mod worker;

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use anyhow::Context;
use clap::Parser;
use pichainboot_proto::Config;
use tracing_subscriber::EnvFilter;

use crate::transport::FlowControl;
use crate::worker::Settings;

/// Load and boot images on a Raspberry Pi running the pichainboot chainloader
#[derive(Debug, Parser)]
#[command(version)]
struct Args {
    /// Image to load. It is read again before every boot.
    image: PathBuf,

    /// Serial port, or tcp:HOST:PORT for a serial port bridged over TCP (e.g. QEMU)
    #[arg(short, long)]
    port: String,

    #[arg(short, long, default_value_t = 115200)]
    baud: u32,

    #[arg(long, value_enum, default_value_t = FlowControl::Hardware)]
    flow_control: FlowControl,

    /// Address to load the image at
    #[arg(long, default_value = "0x80000", value_parser = parse_u64)]
    addr: u64,

    /// Address to jump to [default: the load address]
    #[arg(long, value_parser = parse_u64)]
    entry: Option<u64>,

    /// How long to wait for a response before resending a request, in milliseconds
    #[arg(long, default_value_t = 1000)]
    timeout_ms: u64,

    /// How many times a request is resent before giving up
    #[arg(long, default_value_t = 5)]
    retries: u32,

    /// Boot every time the device connects, e.g. after pressing its reset button
    #[arg(short, long)]
    auto: bool,

    /// No TUI: boot once, then act as a plain serial console on stdin/stdout
    #[arg(long)]
    headless: bool,

    /// Write debug logs to this file
    #[arg(long)]
    log_file: Option<PathBuf>,
}

fn parse_u64(s: &str) -> Result<u64, String> {
    let result = match s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        Some(hex) => u64::from_str_radix(hex, 16),
        None => s.parse(),
    };
    result.map_err(|e| e.to_string())
}

fn init_logging(args: &Args) -> anyhow::Result<()> {
    if let Some(path) = &args.log_file {
        let file = std::fs::File::create(path).with_context(|| format!("failed to create {}", path.display()))?;
        tracing_subscriber::fmt()
            .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("debug")))
            .with_writer(Mutex::new(file))
            .with_ansi(false)
            .init();
    } else if args.headless {
        tracing_subscriber::fmt()
            .with_env_filter(EnvFilter::from_default_env())
            .with_writer(std::io::stderr)
            .init();
    }
    Ok(())
}

async fn run(args: Args) -> anyhow::Result<()> {
    init_logging(&args)?;

    let settings = Settings {
        image: args.image.clone(),
        addr: args.addr,
        entry: args.entry.unwrap_or(args.addr),
        client: Config {
            response_timeout: Duration::from_millis(args.timeout_ms),
            max_retries: args.retries,
            ..Config::default()
        },
        auto: args.auto,
        boot_first: args.headless,
    };
    let transport = transport::open(&args.port, args.baud, args.flow_control).await?;

    if args.headless {
        headless::run(transport, settings).await
    } else {
        tui::run(transport, settings, args.port).await
    }
}

#[tokio::main]
async fn main() {
    let result = run(Args::parse()).await;
    if let Err(e) = &result {
        eprintln!("error: {e:#}");
    }
    // Exit right away instead of waiting for the blocking stdin reader
    std::process::exit(if result.is_ok() { 0 } else { 1 });
}
