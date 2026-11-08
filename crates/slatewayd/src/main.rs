// SPDX-License-Identifier: GPL-3.0-or-later

//! Slateway host daemon: capture, encode and inject on the PC side.
//! The M0 daemon serves the echo session over the loopback or adb
//! reverse transports; capture and encoding arrive in M1.

pub mod session;

use slateway_core::keys::{KnownTablets, config_dir, load_or_create_host_key};
use slateway_platform::{AdbReverseTransport, DEFAULT_PORT, LoopbackTransport, Transport};

#[tokio::main]
async fn main() {
    if let Err(err) = run().await {
        eprintln!("slatewayd: {err}");
        std::process::exit(1);
    }
}

/// How the tablet reaches the daemon (docs/PROTOCOL.md, "Transports").
#[derive(Debug)]
enum TransportChoice {
    Loopback,
    Adb,
}

struct Args {
    transport: TransportChoice,
    port: u16,
    auto_pair: bool,
    echo: bool,
    config_dir: Option<std::path::PathBuf>,
}

fn parse_args(args: impl Iterator<Item = String>) -> Result<Args, String> {
    let mut transport = TransportChoice::Loopback;
    let mut port = DEFAULT_PORT;
    let mut auto_pair = false;
    let mut echo = false;
    let mut config_dir = None;
    let mut iter = args;
    while let Some(arg) = iter.next() {
        let mut value = || iter.next().ok_or_else(|| format!("{arg} needs a value"));
        match arg.as_str() {
            "--transport" => match value()?.as_str() {
                "loopback" => transport = TransportChoice::Loopback,
                "adb" => transport = TransportChoice::Adb,
                other => return Err(format!("unknown transport {other} (loopback|adb)")),
            },
            "--port" => port = value()?.parse().map_err(|err| format!("--port: {err}"))?,
            "--auto-pair" => auto_pair = true,
            "--echo" => echo = true,
            "--config-dir" => config_dir = Some(std::path::PathBuf::from(value()?)),
            other => return Err(format!("unknown argument {other}")),
        }
    }
    Ok(Args {
        transport,
        port,
        auto_pair,
        echo,
        config_dir,
    })
}

async fn run() -> Result<(), String> {
    let args = parse_args(std::env::args().skip(1))?;

    let dir = match &args.config_dir {
        Some(dir) => dir.clone(),
        None => config_dir().map_err(|err| err.to_string())?,
    };
    let host_key = load_or_create_host_key(&dir).map_err(|err| err.to_string())?;
    let mut known =
        KnownTablets::load(&dir.join("known-tablets")).map_err(|err| err.to_string())?;
    println!(
        "slatewayd: host key loaded from {}; transport {:?} on port {}, echo {}, pairing {}",
        dir.display(),
        args.transport,
        args.port,
        if args.echo { "on" } else { "off" },
        if args.auto_pair { "auto" } else { "prompted" },
    );

    let cfg = session::SessionConfig {
        echo: args.echo,
        tick: std::time::Duration::from_secs(1),
    };

    loop {
        // One transport per connection: the loopback transport hands
        // its listener over on accept, and adb reverse is re-registered
        // per connection (the tunnel drops with the device).
        let mut transport: Box<dyn Transport> = match args.transport {
            TransportChoice::Loopback => Box::new(LoopbackTransport::with_port(args.port)),
            TransportChoice::Adb => Box::new(AdbReverseTransport::with_port(args.port)),
        };
        let addr = transport.prepare().await.map_err(|err| err.to_string())?;
        if let Some(addr) = addr {
            println!("slatewayd: waiting for the tablet on {addr}");
        }
        let stream = transport.stream().await.map_err(|err| err.to_string())?;
        let auto_pair = args.auto_pair;
        let confirm = move |code: &str| {
            if auto_pair {
                println!("pairing request: code {code} - auto-paired (--auto-pair)");
                return true;
            }
            println!("pairing request: code {code} - confirm on the tablet, then enter y here");
            let mut line = String::new();
            std::io::stdin().read_line(&mut line).is_ok() && line.trim().eq_ignore_ascii_case("y")
        };
        match session::serve_once(stream, &host_key.private, &mut known, &cfg, &confirm).await {
            Ok(report) => println!(
                "session with '{}' ended: paired {}, echoed {} frames, \
                 frames in {} / out {}, bytes in {} / out {}, \
                 clock rtt {} us, offset {} us, tablet stats {:?}",
                report.device_name,
                report.paired,
                report.echoed,
                report.stats.frames_in,
                report.stats.frames_out,
                report.stats.bytes_in,
                report.stats.bytes_out,
                report.clock.estimate().map(|s| s.rtt_us).unwrap_or(-1),
                report.clock.estimate().map(|s| s.offset_us).unwrap_or(0),
                report.stats.tablet,
            ),
            Err(err) => eprintln!("session failed: {err}"),
        }
        // Reconnect loop: the tablet may return without a new process.
    }
}
