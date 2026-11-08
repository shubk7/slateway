// SPDX-License-Identifier: GPL-3.0-or-later

//! The echo test channel: experimental range (docs/PROTOCOL.md,
//! "Channels": 16 and above are experimental and not stable). Used by
//! `tools/mock-tablet` and the daemon's echo session to measure round
//! trips with opaque payloads; not part of the stable protocol.

/// Experimental channel id for echo test traffic.
pub const CHANNEL: u8 = 200;

/// Message type within the echo channel; opaque to the protocol.
pub const MESSAGE_TYPE: u16 = 1;
