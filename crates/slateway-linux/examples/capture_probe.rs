// SPDX-License-Identifier: GPL-3.0-or-later

//! Manual capture verification for the Linux portal path.
//!
//! Run from a terminal inside the desktop session (Wayland + XDG
//! portals required): `cargo run -p slateway-linux --example capture_probe`.
//! Approve the screen-share prompt once, watch the frame stats for ten
//! seconds, and find one decoded frame as a PPM image under
//! /tmp/opencode/capture-probe.ppm (open it to check the pixels are
//! really the desktop, not garbage).
//!
//! There is no automated variant: a consent dialog needs a session.

fn main() {
    #[cfg(target_os = "linux")]
    {
        if let Err(e) = run() {
            eprintln!("capture probe failed: {e}");
            std::process::exit(1);
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        eprintln!("capture probe is linux-only");
        std::process::exit(1);
    }
}

#[cfg(target_os = "linux")]
fn run() -> Result<(), Box<dyn std::error::Error>> {
    use std::time::{Duration, Instant};

    use slateway_linux::PortalCapture;
    use slateway_platform::capture::{CaptureConfig, CaptureSource};

    let mut capture = PortalCapture::default();
    let info = capture.start(&CaptureConfig {
        max_fps: 60,
        restore_token: None,
    })?;
    println!("capturing {info:?}; dumping frame 3 to /tmp/opencode/capture-probe.ppm");

    let started = Instant::now();
    let mut frames = 0u64;
    let mut last_pts = 0u64;
    while started.elapsed() < Duration::from_secs(10) {
        if let Some(frame) = capture.next_frame(Duration::from_millis(500))? {
            frames += 1;
            println!(
                "frame {frames}: {}x{} stride {} {:?} len {} delta {} us",
                frame.width,
                frame.height,
                frame.stride,
                frame.format,
                frame.data.len(),
                frame.pts_us - last_pts
            );
            last_pts = frame.pts_us;
            if frames == 3 {
                dump_ppm(&frame, "/tmp/opencode/capture-probe.ppm")?;
                println!("wrote /tmp/opencode/capture-probe.ppm");
            }
        }
    }
    capture.stop();
    println!("{frames} frames in 10 s");
    Ok(())
}

#[cfg(target_os = "linux")]
fn dump_ppm(
    frame: &slateway_platform::capture::CaptureFrame,
    path: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    use std::io::Write;

    use slateway_platform::capture::FrameFormat;

    if !matches!(frame.format, FrameFormat::Bgrx | FrameFormat::Rgbx) {
        return Err(format!("ppm dump needs a 4-byte RGB format, got {frame:?}").into());
    }
    // BGRx arrives in B,G,R,A order and must be swapped for PPM.
    let swap = matches!(frame.format, FrameFormat::Bgrx);
    let mut out = Vec::with_capacity(frame.width as usize * frame.height as usize * 3 + 64);
    write!(out, "P6\n{} {}\n255\n", frame.width, frame.height)?;
    for y in 0..frame.height as usize {
        let row = &frame.data[y * frame.stride..];
        for x in 0..frame.width as usize {
            let px = &row[x * 4..x * 4 + 4];
            let (r, g, b) = if swap {
                (px[2], px[1], px[0])
            } else {
                (px[0], px[1], px[2])
            };
            out.extend_from_slice(&[r, g, b]);
        }
    }
    std::fs::create_dir_all("/tmp/opencode")?;
    std::fs::write(path, out)?;
    Ok(())
}
