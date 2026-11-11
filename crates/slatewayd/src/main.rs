// SPDX-License-Identifier: GPL-3.0-or-later

//! Slateway host daemon: capture, encode and inject on the PC side.
//! The M0 daemon serves the echo session over the loopback or adb
//! reverse transports; capture and encoding arrive in M1.

pub mod session;

use slateway_core::keys::{KnownTablets, config_dir, load_or_create_host_key};
use slateway_platform::{AdbReverseTransport, DEFAULT_PORT, LoopbackTransport, Transport};
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

/// How long an idle accept wait runs before the loop cycles: while no
/// session holds the tunnel, the adb reverse registration is refreshed
/// on every cycle because the rule dies with the USB connection (a
/// tablet reconnecting after a replug must find it live again).
const TUNNEL_REFRESH: std::time::Duration = std::time::Duration::from_secs(2);

/// Delay before retrying a failed transport setup or accept.
const RETRY_DELAY: std::time::Duration = std::time::Duration::from_secs(2);

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

/// Keeps the live-session count honest even if a session task panics:
/// the count gates the adb reverse refresh, and a leaked count would
/// disable the tunnel refresh for the life of the process.
struct SessionGuard {
    sessions: Arc<AtomicUsize>,
}

impl SessionGuard {
    fn new(sessions: Arc<AtomicUsize>) -> Self {
        sessions.fetch_add(1, Ordering::Relaxed);
        SessionGuard { sessions }
    }
}

impl Drop for SessionGuard {
    fn drop(&mut self) {
        self.sessions.fetch_sub(1, Ordering::Relaxed);
    }
}

/// Runs the daemon's reconnect loop.
async fn run() -> Result<(), String> {
    let args = parse_args(std::env::args().skip(1))?;

    let dir = match &args.config_dir {
        Some(dir) => dir.clone(),
        None => config_dir().map_err(|err| err.to_string())?,
    };
    let host_key = load_or_create_host_key(&dir).map_err(|err| err.to_string())?;
    let known = KnownTablets::load(&dir.join("known-tablets")).map_err(|err| err.to_string())?;
    for (line, problem) in known.malformed() {
        eprintln!("slatewayd: known-tablets line {line} skipped: {problem}");
    }
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

    // One persistent transport: the listener stays bound across
    // sessions (a tablet reconnecting while a pairing prompt is open
    // must not dial a dead port).
    let mut transport: Box<dyn Transport> = match args.transport {
        TransportChoice::Loopback => Box::new(LoopbackTransport::with_port(args.port)),
        TransportChoice::Adb => Box::new(AdbReverseTransport::with_port(args.port)),
    };
    let known: session::SharedTablets = Arc::new(tokio::sync::Mutex::new(known));
    // Live session count. While zero, the loop refreshes the adb
    // reverse registration every cycle (cable replug drops the rule);
    // while a session holds the link, the tunnel is proven alive, so
    // the loop just keeps cycling without touching adb.
    let sessions = Arc::new(AtomicUsize::new(0));
    let mut announced: Option<SocketAddr> = None;
    println!(
        "waiting for the tablet ({:?} transport, port {})…",
        args.transport, args.port
    );

    loop {
        if sessions.load(Ordering::Relaxed) == 0 {
            match transport.prepare().await {
                Ok(Some(addr)) => {
                    if announced != Some(addr) {
                        println!("listening on {addr}");
                        announced = Some(addr);
                    }
                }
                Ok(None) => {}
                Err(err) => {
                    eprintln!("transport setup failed: {err}; retrying in 2 s");
                    tokio::time::sleep(RETRY_DELAY).await;
                    continue;
                }
            }
        }
        // Bounded accept so the loop keeps cycling even while waiting.
        let stream = match tokio::time::timeout(TUNNEL_REFRESH, transport.stream()).await {
            Ok(Ok(stream)) => stream,
            Ok(Err(err)) => {
                eprintln!("accept failed: {err}; retrying in 2 s");
                tokio::time::sleep(RETRY_DELAY).await;
                continue;
            }
            Err(_elapsed) => continue,
        };
        eprintln!("session: tablet connected");
        // The session runs as its own task: the loop returns to accept
        // immediately, so a pairing prompt never freezes the daemon.
        let known = Arc::clone(&known);
        let cfg = cfg.clone();
        let host_private = host_key.private;
        let auto_pair = args.auto_pair;
        let sessions = Arc::clone(&sessions);
        tokio::spawn(async move {
            // Counts this session for the tunnel-refresh gate; the
            // guard keeps the count honest even if the task panics.
            let _session = SessionGuard::new(sessions);
            let confirm = move |code: &str| {
                if auto_pair {
                    println!("pairing request: code {code} - auto-paired (--auto-pair)");
                    return true;
                }
                // Stray keystrokes re-prompt instead of killing the
                // session; the tablet may already be showing its side
                // of the code. Three tries, then the pairing is
                // refused.
                for attempt in 1..=3 {
                    println!(
                        "pairing request: code {code} - confirm on the tablet, then enter y here ({attempt}/3)"
                    );
                    let mut line = String::new();
                    match std::io::stdin().read_line(&mut line) {
                        Ok(0) => return false, // stdin closed
                        Ok(_) => match line.trim().to_ascii_lowercase().as_str() {
                            "y" | "yes" => return true,
                            "n" | "no" => return false,
                            _ => println!("enter y (confirm) or n (refuse)"),
                        },
                        Err(_) => return false,
                    }
                }
                false
            };
            match session::serve_once(stream, &host_private, &known, &cfg, &confirm).await {
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
            eprintln!("session: tablet disconnected; waiting for the next connection");
        });
        // Reconnect loop: the tablet may return without a new process.
    }
}
