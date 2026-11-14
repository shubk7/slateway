// SPDX-License-Identifier: GPL-3.0-or-later

//! Linux encoder selection: the shared FFmpeg plumbing
//! (`slateway-ffmpeg`) driven by this platform's candidate list.
//! VAAPI needs hw-frame contexts the `ffmpeg-next` wrapper does not
//! expose; it is a pending decision (ADR-014) and arrives with the
//! video-channel work if the latency budget needs it.

use slateway_platform::encoder::{Encoder, EncoderConfig, EncoderError, EncoderInfo};

/// Hardware first (NVENC works on both GPU vendors and takes
/// system-memory frames), software fallback. Selection order per
/// `docs/PLATFORMS.md`, "Linux".
const CANDIDATES: [&str; 2] = ["h264_nvenc", "libx264"];

/// Open an encoder for the given configuration. Blocks while
/// initializing the chosen encoder.
pub fn start(config: &EncoderConfig) -> Result<(Box<dyn Encoder>, EncoderInfo), EncoderError> {
    slateway_ffmpeg::start(config, &CANDIDATES)
}
