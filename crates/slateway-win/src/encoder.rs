// SPDX-License-Identifier: GPL-3.0-or-later

//! Windows encoder selection: the shared FFmpeg plumbing
//! (`slateway-ffmpeg`) driven by this platform's candidate list.
//! `h264_qsv` and `h264_amf` from `docs/PLATFORMS.md`, "Windows", stay
//! pending until their input-format handling is verified (DECISIONS.md
//! pending list).

use slateway_platform::encoder::{Encoder, EncoderConfig, EncoderError, EncoderInfo};

/// Hardware first (NVENC takes system-memory frames and uploads
/// internally), software fallback. Selection order per
/// `docs/PLATFORMS.md`, "Windows".
const CANDIDATES: [&str; 2] = ["h264_nvenc", "libx264"];

/// Open an encoder for the given configuration. Blocks while
/// initializing the chosen encoder.
pub fn start(config: &EncoderConfig) -> Result<(Box<dyn Encoder>, EncoderInfo), EncoderError> {
    slateway_ffmpeg::start(config, &CANDIDATES)
}
