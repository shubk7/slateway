// SPDX-License-Identifier: GPL-3.0-or-later

//! Manual capture verification for the Windows Graphics.Capture path.
//!
//! Run on Windows from a terminal:
//! `cargo run -p slateway-win --example capture_probe`.
//! Watch the frame stats for ten seconds and find one copied frame as
//! a PPM image in the system temp dir (`slateway-capture-probe.ppm`;
//! open it to check the pixels are really the desktop, not garbage).
//!
//! There is no automated variant: this is the first-boot check for the
//! Windows backend, mirroring `slateway-linux`'s probe.

fn main() {
    #[cfg(target_os = "windows")]
    {
        if let Err(e) = run() {
            eprintln!("capture probe failed: {e}");
            std::process::exit(1);
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        eprintln!("capture probe is windows-only");
        std::process::exit(1);
    }
}

#[cfg(target_os = "windows")]
fn run() -> Result<(), Box<dyn std::error::Error>> {
    use std::time::{Duration, Instant};

    use slateway_platform::capture::{CaptureConfig, CaptureSource};
    use slateway_win::WgcCapture;

    let out_path = std::env::temp_dir().join("slateway-capture-probe.ppm");
    let mut capture = WgcCapture::default();
    let info = capture.start(&CaptureConfig {
        max_fps: 60,
        restore_token: None,
    })?;
    println!(
        "capturing {info:?}; dumping frame 3 to {}",
        out_path.display()
    );

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
                dump_ppm(&frame, &out_path)?;
                println!("wrote {}", out_path.display());
            }
        }
    }
    capture.stop();
    println!("{frames} frames in 10 s");
    Ok(())
}

#[cfg(target_os = "windows")]
fn dump_ppm(
    frame: &slateway_platform::capture::CaptureFrame,
    path: &std::path::Path,
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
    std::fs::write(path, out)?;
    Ok(())
}
