// SPDX-License-Identifier: GPL-3.0-or-later

//! Fuzz target for the input batch parser (`docs/PROTOCOL.md`, "Input
//! event").
//!
//! Invariants exercised on arbitrary bytes: parsing never panics, every
//! accepted event re-encodes to exactly the 16 bytes it was parsed
//! from, and iteration stops cleanly at the first error.

#![no_main]

use libfuzzer_sys::fuzz_target;
use slateway_proto::input::{parse_batch, EVENT_LEN};

fuzz_target!(|data: &[u8]| {
    for (index, event) in parse_batch(data).enumerate() {
        match event {
            Ok(event) => {
                let start = index * EVENT_LEN;
                assert_eq!(
                    event.encode().as_slice(),
                    &data[start..start + EVENT_LEN]
                );
            }
            Err(_) => break,
        }
    }
});
