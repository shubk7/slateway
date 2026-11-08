// SPDX-License-Identifier: GPL-3.0-or-later

//! Scripted tablet client CLI (docs/ARCHITECTURE.md, tools/mock-tablet).
//!
//! ```text
//! mock-tablet --addr 127.0.0.1:47820 --frames 100 --size 1024 \
//!             --interval-ms 10 [--name mock-tablet] [--key-file path]
//! ```

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use mock_tablet::{ClientConfig, run_client};

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let args = match parse_args(std::env::args().skip(1)) {
        Ok(args) => args,
        Err(problem) => {
            eprintln!(
                "usage: mock-tablet [--addr host:port] [--frames n] [--size bytes] \
                       [--interval-ms ms] [--name name] [--key-file path] [--pairing-code]\n{problem}"
            );
            std::process::exit(2);
        }
    };

    let stream = match tokio::net::TcpStream::connect(args.addr).await {
        Ok(stream) => {
            // Echo RTTs need TCP_NODELAY (docs/ARCHITECTURE.md, latency
            // rule 5).
            let _ = stream.set_nodelay(true);
            stream
        }
        Err(err) => {
            eprintln!("connect to {}: {err}", args.addr);
            std::process::exit(1);
        }
    };

    let mut cfg = ClientConfig {
        addr: args.addr,
        frames: args.frames,
        size: args.size,
        interval: Duration::from_millis(args.interval_ms),
        device_name: args.name.clone(),
        private_key: None,
    };
    if let Some(path) = &args.key_file {
        match load_or_create_key(path) {
            Ok(key) => cfg.private_key = Some(key),
            Err(err) => {
                eprintln!("key file {}: {err}", path.display());
                std::process::exit(1);
            }
        }
    }

    match run_client(&cfg, stream).await {
        Ok(summary) => {
            let (p50, p95) = summary.rtt_percentiles();
            println!(
                "paired with code {} (confirm on the host if prompted)",
                summary.pairing_code
            );
            println!("host: {}", summary.host_version);
            println!(
                "echo: {} frames of {} bytes, rtt p50 {} us, p95 {} us",
                summary.frames,
                cfg.size,
                p50.map(|v| v.to_string()).unwrap_or_else(|| "-".into()),
                p95.map(|v| v.to_string()).unwrap_or_else(|| "-".into()),
            );
            println!(
                "counters: in {} frames / {} bytes, out {} frames / {} bytes",
                summary.stats.frames_in,
                summary.stats.bytes_in,
                summary.stats.frames_out,
                summary.stats.bytes_out,
            );
        }
        Err(err) => {
            eprintln!("mock-tablet failed: {err}");
            std::process::exit(1);
        }
    }
}

struct Args {
    addr: SocketAddr,
    frames: usize,
    size: usize,
    interval_ms: u64,
    name: String,
    key_file: Option<PathBuf>,
}

/// Hand-rolled parsing; the mock has no dependency budget for a CLI
/// framework.
fn parse_args(args: impl Iterator<Item = String>) -> Result<Args, String> {
    let mut addr = SocketAddr::from(([127, 0, 0, 1], 47820));
    let mut frames = 100usize;
    let mut size = 1024usize;
    let mut interval_ms = 10u64;
    let mut name = "mock-tablet".to_string();
    let mut key_file = None;

    let mut iter = args;
    while let Some(arg) = iter.next() {
        let mut value = |expect: &str| -> Result<String, String> {
            iter.next()
                .ok_or_else(|| format!("{arg} needs a value ({expect})"))
        };
        match arg.as_str() {
            "--addr" => {
                let raw = value("host:port")?;
                addr = raw.parse().map_err(|err| format!("--addr {raw}: {err}"))?;
            }
            "--frames" => {
                frames = value("count")?
                    .parse()
                    .map_err(|err| format!("--frames: {err}"))?
            }
            "--size" => {
                size = value("bytes")?
                    .parse()
                    .map_err(|err| format!("--size: {err}"))?
            }
            "--interval-ms" => {
                interval_ms = value("ms")?
                    .parse()
                    .map_err(|err| format!("--interval-ms: {err}"))?
            }
            "--name" => name = value("name")?,
            "--key-file" => key_file = Some(PathBuf::from(value("path")?)),
            other => return Err(format!("unknown argument {other}")),
        }
    }
    Ok(Args {
        addr,
        frames,
        size,
        interval_ms,
        name,
        key_file,
    })
}

/// Reads the raw 32-byte private key, creating it on first use so
/// repeated runs against a pairing host keep one identity.
fn load_or_create_key(path: &std::path::Path) -> Result<[u8; 32], String> {
    use std::io::Write;
    if path.exists() {
        let raw = std::fs::read(path).map_err(|err| err.to_string())?;
        return raw
            .as_slice()
            .try_into()
            .map_err(|_| "key file must hold exactly 32 bytes".to_string());
    }
    let (private, _public) =
        slateway_core::crypto::generate_keypair().map_err(|err| err.to_string())?;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|err| err.to_string())?;
    file.write_all(&private).map_err(|err| err.to_string())?;
    private
        .try_into()
        .map_err(|_| "generated key has the wrong length".to_string())
}
