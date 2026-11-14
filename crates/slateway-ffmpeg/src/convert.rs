// SPDX-License-Identifier: GPL-3.0-or-later

//! RGB → yuv420p conversion in safe Rust.
//!
//! libswscale would normally do this, but its context is a raw pointer
//! (`!Send`) and the encoder object must travel with the session's
//! video task; a scalar conversion is inside the M1 latency budget.
//! BT.601 with limited range (Y 16-235, chroma 16-240), the classic
//! assumption for H.264 without explicit colorimetry signaling. The
//! 16.16 fixed-point coefficients are the ITU-R BT.601 matrix scaled
//! by 65536/255; they are exact enough to hit the documented range
//! endpoints (white → 235/128/128, blue → 16/240/128) without clamping,
//! but clamp anyway so a coefficient change cannot wrap.
//!
//! The two passes exist because the plane accessor borrows the whole
//! frame: pass one fills luma, pass two chroma, one plane at a time.

use slateway_platform::capture::{CaptureFrame, FrameFormat};

use super::ffmpeg::util::frame::Video as AvFrame;

// 16.16 fixed point.
const COEFF_Y_R: i32 = 16829; // 65.481 / 255 * 65536
const COEFF_Y_G: i32 = 33046; // 128.553 / 255 * 65536
const COEFF_Y_B: i32 = 6417; // 24.966 / 255 * 65536
const COEFF_CB_R: i32 = -9719; // -37.797 / 255 * 65536
const COEFF_CB_G: i32 = -19077; // -74.203 / 255 * 65536
const COEFF_CB_B: i32 = 28770; // 112.0 / 255 * 65536
const COEFF_CR_R: i32 = 28770; // 112.0 / 255 * 65536
const COEFF_CR_G: i32 = -24089; // -93.786 / 255 * 65536
const COEFF_CR_B: i32 = -4681; // -18.214 / 255 * 65536
const OFFSET_Y: i32 = 16 << 16; // limited range black at Y = 16
const OFFSET_C: i32 = 128 << 16; // neutral chroma at 128
const ROUND: i32 = (1 << 16) / 2;

/// Convert one RGB sample triple to limited-range yuv (Y, Cb, Cr).
#[inline]
fn rgb_to_yuv(r: i32, g: i32, b: i32) -> (u8, u8, u8) {
    let y =
        ((OFFSET_Y + COEFF_Y_R * r + COEFF_Y_G * g + COEFF_Y_B * b + ROUND) >> 16).clamp(0, 255);
    let cb =
        ((OFFSET_C + COEFF_CB_R * r + COEFF_CB_G * g + COEFF_CB_B * b + ROUND) >> 16).clamp(0, 255);
    let cr =
        ((OFFSET_C + COEFF_CR_R * r + COEFF_CR_G * g + COEFF_CR_B * b + ROUND) >> 16).clamp(0, 255);
    (y as u8, cb as u8, cr as u8)
}

/// Byte offsets of R, G and B inside one 4-byte pixel for a capture
/// format (BGRx's ignored fourth byte rides along; RGBA and RGBX pack
/// the same channel order).
#[inline]
fn channel_offsets(format: FrameFormat) -> (usize, usize, usize) {
    match format {
        // Memory order B, G, R, pad.
        FrameFormat::Bgrx | FrameFormat::Rgbx => (2, 1, 0),
        // Memory order R, G, B, pad.
        FrameFormat::Rgba | FrameFormat::Rgb => (0, 1, 2),
    }
}

/// Sample the R, G and B channels of one 4-byte pixel as i32.
#[inline]
fn sample(row: &[u8], x: usize, offsets: (usize, usize, usize)) -> (i32, i32, i32) {
    let (r_off, g_off, b_off) = offsets;
    let at = x * 4;
    (
        row[at + r_off] as i32,
        row[at + g_off] as i32,
        row[at + b_off] as i32,
    )
}

/// Convert a capture frame into the yuv420p planes of `dst`. Geometry
/// must match (`dst` carries the encoder config's geometry) and both
/// must be even; the encoder setup rejects odd dimensions.
pub fn rgb_to_yuv420(
    frame: &CaptureFrame,
    format: FrameFormat,
    dst: &mut AvFrame,
) -> Result<(), String> {
    let width = frame.width as usize;
    let height = frame.height as usize;
    debug_assert!(width.is_multiple_of(2) && height.is_multiple_of(2));
    let offsets = channel_offsets(format);

    let y_stride = dst.stride(0);
    let c_stride = dst.stride(1);
    let src_stride = frame.stride;
    if dst.data(0).len() < (height - 1) * y_stride + width
        || dst.data(1).len() < (height / 2 - 1) * c_stride + width / 2
        || dst.data(2).len() < (height / 2 - 1) * c_stride + width / 2
    {
        return Err(format!("yuv420p planes too small for {width}x{height}"));
    }

    let src = &frame.data;

    // Pass one: luma, one pixel each.
    {
        let y_plane = dst.data_mut(0);
        for (y, row) in src.chunks_exact(src_stride).take(height).enumerate() {
            let dst_row = &mut y_plane[y * y_stride..][..width];
            for (x, dst_at) in dst_row.iter_mut().enumerate() {
                let (r, g, b) = sample(row, x, offsets);
                let (y, _, _) = rgb_to_yuv(r, g, b);
                *dst_at = y;
            }
        }
    }

    // Pass two: chroma from the 2x2 box average, one plane per pass
    // (the plane accessor borrows the whole frame).
    for plane in [1usize, 2] {
        let chroma = dst.data_mut(plane);
        for cy in 0..height / 2 {
            let y0 = cy * 2;
            let src_row0 = &src[y0 * src_stride..];
            let src_row1 = &src[(y0 + 1) * src_stride..];
            let dst_row = &mut chroma[cy * c_stride..][..width / 2];
            for (cx, dst_at) in dst_row.iter_mut().enumerate() {
                let x = cx * 2;
                let p00 = sample(src_row0, x, offsets);
                let p01 = sample(src_row0, x + 1, offsets);
                let p10 = sample(src_row1, x, offsets);
                let p11 = sample(src_row1, x + 1, offsets);
                let r = (p00.0 + p01.0 + p10.0 + p11.0 + 2) / 4;
                let g = (p00.1 + p01.1 + p10.1 + p11.1 + 2) / 4;
                let b = (p00.2 + p01.2 + p10.2 + p11.2 + 2) / 4;
                let (_, cb, cr) = rgb_to_yuv(r, g, b);
                *dst_at = if plane == 1 { cb } else { cr };
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{AvFrame, rgb_to_yuv, rgb_to_yuv420};
    use ffmpeg_next::util::format::Pixel;
    use slateway_platform::capture::{CaptureFrame, FrameFormat};

    #[test]
    fn limited_range_endpoints_hold() {
        // White and the primaries land on the documented endpoints.
        let (y, cb, cr) = rgb_to_yuv(255, 255, 255);
        assert_eq!((y, cb, cr), (235, 128, 128));
        let (y, _, _) = rgb_to_yuv(0, 0, 0);
        assert_eq!(y, 16);
        let (_, cb, _) = rgb_to_yuv(0, 0, 255);
        assert_eq!(cb, 240);
        let (_, _, cr) = rgb_to_yuv(255, 0, 0);
        assert_eq!(cr, 240);
    }

    /// End-to-end: converted planes in the output frame (checked
    /// directly against a solid-color frame; mid gray maps to
    /// Y = 126 with the BT.601 limited matrix, chroma neutral).
    #[test]
    fn solid_gray_frame_converts_to_flat_planes() {
        let width: u32 = 64;
        let height: u32 = 32;
        let frame = CaptureFrame {
            width,
            height,
            stride: width as usize * 4,
            format: FrameFormat::Bgrx,
            // Mid gray.
            data: [128, 128, 128, 255].repeat(width as usize * height as usize),
            pts_us: 0,
        };
        let mut dst = AvFrame::new(Pixel::YUV420P, width, height);
        rgb_to_yuv420(&frame, FrameFormat::Bgrx, &mut dst).expect("test");
        assert!(dst.data(0).iter().all(|&v| v == 126));
        assert!(dst.data(1).iter().all(|&v| v == 128));
        assert!(dst.data(2).iter().all(|&v| v == 128));
    }
}
