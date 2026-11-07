// SPDX-License-Identifier: GPL-3.0-or-later

//! Fuzz target for the control-message decoders (`docs/PROTOCOL.md`,
//! "Control schemas").
//!
//! Invariants exercised on arbitrary bytes: prost decoding never
//! panics, and every accepted message survives a re-encode-redecode
//! cycle unchanged (semantic round-trip; proto3 re-encoding is not
//! byte-identical because zero fields get dropped).

#![no_main]

use libfuzzer_sys::fuzz_target;
use prost::Message;
use slateway_proto::control::{Close, Hello, HelloAck};

fn round_trips<M: Message + PartialEq + Default + std::fmt::Debug>(data: &[u8]) {
    if let Ok(msg) = M::decode(data) {
        let re_encoded = msg.encode_to_vec();
        match M::decode(re_encoded.as_slice()) {
            Ok(decoded_again) => assert_eq!(msg, decoded_again),
            Err(err) => panic!("re-encode of a decoded message failed to decode: {err}"),
        }
    }
}

fuzz_target!(|data: &[u8]| {
    round_trips::<Hello>(data);
    round_trips::<HelloAck>(data);
    round_trips::<Close>(data);
});
