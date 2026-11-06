// SPDX-License-Identifier: GPL-3.0-or-later

//! Fuzz target for the frame header parser (`docs/PROTOCOL.md`, "Frame
//! header").
//!
//! Invariants exercised on arbitrary bytes: parsing never panics, a
//! buffer shorter than a header is always rejected, and every accepted
//! header re-encodes to exactly the bytes it was parsed from.

#![no_main]

use libfuzzer_sys::fuzz_target;
use slateway_proto::{FrameHeader, HEADER_LEN};

fuzz_target!(|data: &[u8]| {
    if data.len() < HEADER_LEN {
        assert!(FrameHeader::parse(data).is_err());
        return;
    }
    if let Ok(header) = FrameHeader::parse(data) {
        assert_eq!(header.encode().as_slice(), &data[..HEADER_LEN]);
    }
});
