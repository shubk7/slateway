// SPDX-License-Identifier: GPL-3.0-or-later

//! Slateway sessions, channels, pairing, clock sync and stats.
//!
//! Platform-agnostic: builds and tests on every OS with no platform
//! code (platform backends live behind the traits in
//! `slateway-platform`).

pub mod crypto;
pub mod keys;

pub use crypto::{
    CIPHER_SUITE, HandshakeDone, KEY_LEN, MAX_PLAINTEXT, MAX_RECORD, SecureSession, SessionError,
    SessionHandshake, pairing_code,
};
pub use keys::{HostKey, KeysError, KnownTablet, KnownTablets, config_dir};
