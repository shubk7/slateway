// SPDX-License-Identifier: GPL-3.0-or-later

//! Slateway wire codec and schemas, no I/O.
//!
//! Implements the layouts fixed in `docs/PROTOCOL.md` (draft v0):
//!
//! - [`frame`]: the 16-byte frame header every frame carries.
//! - [`channel`]: channel identifiers.
//! - [`input`]: the 16-byte packed input event and its batches.
//!
//! Everything here is I/O-free and allocation-free. Parsers validate
//! before any size-dependent work and never panic on untrusted bytes.
//! Control-message Protobuf schemas are designed with the handshake
//! work and are not part of this crate yet.

pub mod channel;
pub mod control;
pub mod frame;
pub mod input;

pub use channel::{Channel, UnknownChannelError};
pub use control::{
    CLOSE_TYPE, Close, CloseReason, DecodeLimits, HELLO_ACK_TYPE, HELLO_TYPE, Hello, HelloAck,
    PAIR_NEEDED_TYPE, PING_TYPE, PONG_TYPE, PairNeeded, Ping, Pong, STATS_TYPE, Screen, Stats,
    VideoCodec,
};
pub use frame::{FrameError, FrameHeader, HEADER_LEN, MAX_PAYLOAD_LEN};
pub use input::{BatchEncodeError, EVENT_LEN, InputError, InputEvent};
