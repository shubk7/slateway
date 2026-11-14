// SPDX-License-Identifier: GPL-3.0-or-later

//! Manual encoder verification: capture the desktop through the portal
//! path and push every frame through the FFmpeg encoder.
//!
//! Run from a terminal inside the desktop session:
//! `cargo run --release -p slateway-linux --example encoder_probe`.
//! Approve the screen-share prompt once; the probe then captures and
//! encodes for ten seconds, prints per-stage latency stats, and writes
//! the elementary stream to /tmp/opencode/encoder-probe.h264 (verify
//! with `ffprobe` / play it back to check the picture is the desktop).
//!
//! There is no automated variant: a consent dialog needs a session.

fn main() {
    #[cfg(target_os = "linux")]
    {
        if let Err(e) = run() {
            eprintln!("encoder probe failed: {e}");
            std::process::exit(1);
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        eprintln!("encoder probe is linux-only");
        std::process::exit(1);
    }
}

#[cfg(target_os = "linux")]
fn run() -> Result<(), Box<dyn std::error::Error>> {
    use std::io::Write;
    use std::time::{Duration, Instant};

    use slateway_linux::PortalCapture;
    use slateway_linux::encoder;
    use slateway_platform::capture::{CaptureConfig, CaptureSource};
    use slateway_platform::encoder::EncoderConfig;

    const RUN_SECS: u64 = 10;

    let mut capture = Box::new(PortalCapture::default());
    let capture_info = capture.start(&CaptureConfig {
        max_fps: 60,
        restore_token: None,
    })?;
    println!("capturing {capture_info:?}");

    let encoder_config = EncoderConfig {
        width: capture_info.width,
        height: capture_info.height,
        fps: capture_info.fps,
        bitrate_kbps: 0,
    };
    let (mut encoder, encoder_info) = encoder::start(&encoder_config)?;
    println!(
        "encoding with {} (hint {} bytes/packet); writing /tmp/opencode/encoder-probe.h264",
        encoder_info.codec, encoder_info.max_packet_bytes_hint
    );
    std::fs::create_dir_all("/tmp/opencode")?;
    let mut stream = std::fs::File::create("/tmp/opencode/encoder-probe.h264")?;

    let started = Instant::now();
    let mut frames = 0u64;
    let mut packets = 0u64;
    let mut keyframes = 0u64;
    let mut bytes = 0u64;
    let mut encode_us_sum = 0u64;
    let mut encode_us_max = 0u64;
    let mut deltas: Vec<u64> = Vec::new();
    let mut last_frame_pts = 0u64;
    let mut frame = None;
    while started.elapsed() < Duration::from_secs(RUN_SECS) {
        if let Some(new) = capture.next_frame(Duration::from_millis(200))? {
            let encode_started = Instant::now();
            let out = encoder.encode(&new)?;
            let encode_us = encode_started.elapsed().as_micros() as u64;
            encode_us_sum += encode_us;
            encode_us_max = encode_us_max.max(encode_us);
            for packet in &out {
                stream.write_all(&packet.data)?;
                bytes += packet.data.len() as u64;
                if packet.keyframe {
                    keyframes += 1;
                }
                packets += 1;
            }
            deltas.push(new.pts_us - last_frame_pts);
            last_frame_pts = new.pts_us;
            frames += 1;
            frame = Some(new);
        }
    }
    let tail = encoder.flush()?;
    for packet in &tail {
        stream.write_all(&packet.data)?;
        packets += 1;
        bytes += packet.data.len() as u64;
    }
    capture.stop();
    stream.flush()?;

    deltas.sort_unstable();
    let pct = |p: usize| -> u64 {
        if deltas.is_empty() {
            0
        } else {
            deltas[(deltas.len() * p / 100).min(deltas.len() - 1)]
        }
    };
    println!(
        "{frames} frames, {packets} packets ({keyframes} keyframes), {bytes} bytes in {RUN_SECS} s"
    );
    println!(
        "capture frame delta p50 {} us p95 {} us; encode p50 {} us max {} us; avg {:.1} Mbps",
        pct(50),
        pct(95),
        encode_us_sum.checked_div(frames).unwrap_or(0),
        encode_us_max,
        bytes as f64 * 8.0 / RUN_SECS as f64 / 1_000_000.0
    );
    if let Some(f) = frame {
        println!(
            "last frame {}x{} stride {} {:?}",
            f.width, f.height, f.stride, f.format
        );
    }
    Ok(())
}
