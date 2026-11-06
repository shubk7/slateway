// SPDX-License-Identifier: GPL-3.0-or-later

//! Channel identifiers (`docs/PROTOCOL.md`, "Channels").

/// Error for a rejected channel ID: the reserved range 9 to 15
/// (`docs/PROTOCOL.md`, "Channels").
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("unknown channel id {id}")]
pub struct UnknownChannelError {
    /// The rejected channel ID.
    pub id: u8,
}

/// Channel identifier carried in the first byte of every frame header.
///
/// IDs 9 to 15 are reserved and rejected by [`Channel::from_id`]. IDs 16
/// and above are experimental and not stable; they parse successfully so
/// that frames on them can be framed and then safely ignored by
/// consumers that do not know the channel.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Channel {
    /// Protobuf control messages, both directions.
    Control,
    /// H.264 Annex-B access units, PC to tablet.
    Video,
    /// Packed input events, tablet to PC.
    Input,
    /// Opus audio, PC to tablet.
    AudioOut,
    /// Opus audio, tablet to PC.
    AudioIn,
    /// H.264 Annex-B, tablet to PC.
    Camera,
    /// Protobuf clipboard messages, both directions.
    Clipboard,
    /// H.264, tablet to PC (X2).
    TabletScreen,
    /// Reserved for X3 file transfer.
    File,
    /// An experimental channel, ID 16 to 255. Not stable.
    Experimental {
        /// The wire ID, 16 to 255.
        id: u8,
    },
}

impl Channel {
    /// Maps a wire ID to a channel. Reserved IDs 9 to 15 are rejected:
    /// parsers must reject unknown channels (`docs/PROTOCOL.md`, "Frame
    /// header").
    pub fn from_id(id: u8) -> Result<Self, UnknownChannelError> {
        match id {
            0 => Ok(Channel::Control),
            1 => Ok(Channel::Video),
            2 => Ok(Channel::Input),
            3 => Ok(Channel::AudioOut),
            4 => Ok(Channel::AudioIn),
            5 => Ok(Channel::Camera),
            6 => Ok(Channel::Clipboard),
            7 => Ok(Channel::TabletScreen),
            8 => Ok(Channel::File),
            9..=15 => Err(UnknownChannelError { id }),
            16..=255 => Ok(Channel::Experimental { id }),
        }
    }

    /// The channel's wire ID.
    pub fn id(self) -> u8 {
        match self {
            Channel::Control => 0,
            Channel::Video => 1,
            Channel::Input => 2,
            Channel::AudioOut => 3,
            Channel::AudioIn => 4,
            Channel::Camera => 5,
            Channel::Clipboard => 6,
            Channel::TabletScreen => 7,
            Channel::File => 8,
            Channel::Experimental { id } => id,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Cross-checked against the channel table in docs/PROTOCOL.md.
    #[test]
    fn ids_match_spec() {
        assert_eq!(Channel::Control.id(), 0);
        assert_eq!(Channel::Video.id(), 1);
        assert_eq!(Channel::Input.id(), 2);
        assert_eq!(Channel::AudioOut.id(), 3);
        assert_eq!(Channel::AudioIn.id(), 4);
        assert_eq!(Channel::Camera.id(), 5);
        assert_eq!(Channel::Clipboard.id(), 6);
        assert_eq!(Channel::TabletScreen.id(), 7);
        assert_eq!(Channel::File.id(), 8);
    }

    #[test]
    fn round_trip_named_ids() {
        for id in 0..=8 {
            assert_eq!(Channel::from_id(id).map(Channel::id), Ok(id));
        }
    }

    #[test]
    fn round_trip_experimental_ids() {
        for id in 16..=255 {
            match Channel::from_id(id) {
                Ok(Channel::Experimental { id: got }) => assert_eq!(got, id),
                other => panic!("experimental id {id} parsed as {other:?}"),
            }
        }
    }

    #[test]
    fn reserved_ids_rejected() {
        for id in 9..=15 {
            assert_eq!(
                Channel::from_id(id),
                Err(UnknownChannelError { id }),
                "reserved id {id} must be rejected"
            );
        }
    }
}
