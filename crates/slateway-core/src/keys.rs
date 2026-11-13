// SPDX-License-Identifier: GPL-3.0-or-later

//! Host key storage (`docs/SECURITY.md`): the host's static Noise key
//! and the list of paired tablets, under the Slateway config directory.
//!
//! Layout: `host.key` holds the raw 32-byte static private key, mode
//! 0600 on Unix (the Windows profile directory is user-private by
//! default). `known-tablets` holds one paired tablet per line,
//! `<hex public key> <name>`, known_hosts-style and hand-parsed. The
//! tablet learns the host's public key from the Noise handshake, so
//! the host never needs its own public half on disk.

use std::fs;
use std::path::{Path, PathBuf};

use crate::crypto::{KEY_LEN, generate_keypair};

/// Key and pairing-store failures.
#[derive(Debug, thiserror::Error)]
pub enum KeysError {
    /// Reading or writing the config directory failed.
    #[error(transparent)]
    Io(#[from] std::io::Error),
    /// Key generation failed.
    #[error(transparent)]
    Crypto(#[from] crate::crypto::SessionError),
    /// A stored file has an unusable shape.
    #[error("{file}: {problem}")]
    Malformed {
        /// Which file.
        file: &'static str,
        /// What is wrong with it.
        problem: &'static str,
    },
    /// The static key file does not hold exactly [`KEY_LEN`] bytes.
    #[error("host.key holds {len} bytes, expected {expected}")]
    BadKeyLength {
        /// Observed length.
        len: usize,
        /// Expected length.
        expected: usize,
    },
}

/// Resolves the Slateway config directory: `SLATEWAY_CONFIG_DIR`, else
/// `$XDG_CONFIG_HOME/slateway` or the platform default.
pub fn config_dir() -> Result<PathBuf, KeysError> {
    let xdg = std::env::var_os("XDG_CONFIG_HOME");
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"));
    config_dir_from(
        std::env::var_os("SLATEWAY_CONFIG_DIR").as_deref(),
        xdg.as_deref(),
        home.as_deref(),
    )
    .map_err(|problem| KeysError::Malformed {
        file: "config dir",
        problem,
    })
}

/// Pure resolution logic, testable without touching the environment:
/// the override wins, then XDG, then the platform default.
fn config_dir_from(
    override_dir: Option<&std::ffi::OsStr>,
    xdg_config_home: Option<&std::ffi::OsStr>,
    home: Option<&std::ffi::OsStr>,
) -> Result<PathBuf, &'static str> {
    if let Some(dir) = override_dir {
        if dir.is_empty() {
            return Err("SLATEWAY_CONFIG_DIR is empty");
        }
        return Ok(PathBuf::from(dir));
    }
    if let Some(xdg) = xdg_config_home {
        if xdg.is_empty() {
            return Err("XDG_CONFIG_HOME is empty");
        }
        // XDG base-directory spec: a relative $XDG_CONFIG_HOME does
        // not name a config directory and must be ignored - the
        // platform default applies instead of trusting a path whose
        // meaning depends on the process's working directory. A
        // rooted path (`/xdg`) counts as absolute: it does not depend
        // on the process's working directory (and the XDG variable is
        // only ever meaningful on Unix, where rooted == absolute).
        if !std::path::Path::new(xdg).has_root() {
            // fall through to the home-derived default below
        } else {
            return Ok(PathBuf::from(xdg).join("slateway"));
        }
    }
    let home = home.ok_or("no home directory; set SLATEWAY_CONFIG_DIR")?;
    if cfg!(windows) {
        Ok(PathBuf::from(home)
            .join("AppData")
            .join("Roaming")
            .join("slateway"))
    } else {
        Ok(PathBuf::from(home).join(".config").join("slateway"))
    }
}

/// The host's static Noise private key.
pub struct HostKey {
    /// Static private key, 32 bytes.
    pub private: [u8; KEY_LEN],
}

/// Loads `host.key` from `dir`, creating the directory and the key on
/// first run. The key file holds the private key only; the tablet
/// learns the public half from the Noise handshake.
pub fn load_or_create_host_key(dir: &Path) -> Result<HostKey, KeysError> {
    fs::create_dir_all(dir)?;
    let path = dir.join("host.key");
    if path.exists() {
        let raw = fs::read(&path)?;
        if raw.len() != KEY_LEN {
            return Err(KeysError::BadKeyLength {
                len: raw.len(),
                expected: KEY_LEN,
            });
        }
        let mut private = [0u8; KEY_LEN];
        private.copy_from_slice(&raw);
        return Ok(HostKey { private });
    }
    let (private, _public) = generate_keypair()?;
    let mut file = open_private_file(&path)?;
    file.write_all(&private)?;
    file.sync_all()?;
    let private: [u8; KEY_LEN] =
        private
            .as_slice()
            .try_into()
            .map_err(|_| KeysError::BadKeyLength {
                len: private.len(),
                expected: KEY_LEN,
            })?;
    Ok(HostKey { private })
}

/// One paired tablet: static public key and a display name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KnownTablet {
    /// The tablet's static public key.
    pub public: [u8; KEY_LEN],
    /// Display name, e.g. the device model.
    pub name: String,
}

/// The paired-tablet store, backed by the `known-tablets` file.
pub struct KnownTablets {
    path: PathBuf,
    tablets: Vec<KnownTablet>,
    malformed: Vec<(usize, &'static str)>,
}

/// Longest accepted tablet name, in bytes.
const MAX_NAME_LEN: usize = 128;

impl KnownTablets {
    /// Loads the store; a missing file is an empty store. Malformed
    /// lines are skipped and reported via [`KnownTablets::malformed`]
    /// instead of failing the whole store: a lost entry only means
    /// that one tablet pairs again, while a failed load would forget
    /// every pairing.
    pub fn load(path: &Path) -> Result<Self, KeysError> {
        let mut tablets = Vec::new();
        let mut malformed = Vec::new();
        match fs::read_to_string(path) {
            Ok(text) => {
                for (number, line) in text.lines().enumerate() {
                    let line = line.trim();
                    if line.is_empty() || line.starts_with('#') {
                        continue;
                    }
                    match parse_tablet_line(line) {
                        Ok(tablet) => tablets.push(tablet),
                        Err(problem) => malformed.push((number + 1, problem)),
                    }
                }
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => return Err(err.into()),
        }
        Ok(KnownTablets {
            path: path.to_path_buf(),
            tablets,
            malformed,
        })
    }

    /// Lines skipped by [`KnownTablets::load`] as `(line number,
    /// problem)`; the store still holds every parsable entry.
    pub fn malformed(&self) -> &[(usize, &'static str)] {
        &self.malformed
    }

    /// The tablet with this static public key, if paired.
    pub fn find(&self, public: &[u8; KEY_LEN]) -> Option<&KnownTablet> {
        self.tablets.iter().find(|t| &t.public == public)
    }

    /// Adds a tablet (or updates an existing entry's name) and saves
    /// immediately, so a crash cannot forget a just-paired device.
    ///
    /// The name comes from the tablet's handshake payload and becomes
    /// one line of the registry file: control characters (which could
    /// forge extra lines) and oversized names are rejected.
    pub fn insert(&mut self, tablet: KnownTablet) -> Result<(), KeysError> {
        if tablet.name.chars().any(|c| c.is_control()) {
            return Err(KeysError::Malformed {
                file: "known-tablets",
                problem: "tablet name contains control characters",
            });
        }
        if tablet.name.len() > MAX_NAME_LEN {
            return Err(KeysError::Malformed {
                file: "known-tablets",
                problem: "tablet name exceeds 128 bytes",
            });
        }
        match self.tablets.iter_mut().find(|t| t.public == tablet.public) {
            Some(existing) => existing.name = tablet.name,
            None => self.tablets.push(tablet),
        }
        self.save()
    }

    fn save(&self) -> Result<(), KeysError> {
        let mut text = String::new();
        for tablet in &self.tablets {
            text.push_str(&hex_encode(&tablet.public));
            text.push(' ');
            text.push_str(&tablet.name);
            text.push('\n');
        }
        // Atomic replace: a crash mid-write must not truncate the
        // store (the next load would forget every pairing).
        let tmp = self.path.with_extension("tmp");
        {
            let mut file = open_private_file(&tmp)?;
            file.write_all(text.as_bytes())?;
            file.sync_all()?;
        }
        fs::rename(&tmp, &self.path)?;
        Ok(())
    }
}

fn parse_tablet_line(line: &str) -> Result<KnownTablet, &'static str> {
    let (hex_part, name) = line
        .split_once(' ')
        .ok_or("expected '<hex public key> <name>'")?;
    let name = name.trim();
    if name.is_empty() {
        return Err("empty device name");
    }
    let public = hex_decode(hex_part).ok_or("public key is not 64 hex characters")?;
    Ok(KnownTablet {
        public,
        name: name.to_string(),
    })
}

/// Opens a file that only the current user may read, creating it if
/// needed. Mode 0600 on Unix; Windows profile directories are
/// user-private by default.
fn open_private_file(path: &Path) -> Result<fs::File, KeysError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let mut options = fs::OpenOptions::new();
        options.write(true).create(true).truncate(true).mode(0o600);
        Ok(options.open(path)?)
    }
    #[cfg(not(unix))]
    {
        Ok(fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(path)?)
    }
}

use std::io::Write;

/// Lowercase hex, hand-rolled to avoid a dependency.
fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0xf) as usize] as char);
    }
    out
}

/// Decodes exactly `KEY_LEN` bytes of lowercase or uppercase hex.
fn hex_decode(text: &str) -> Option<[u8; KEY_LEN]> {
    if text.len() != KEY_LEN * 2 {
        return None;
    }
    let mut out = [0u8; KEY_LEN];
    for (i, pair) in text.as_bytes().chunks(2).enumerate() {
        let hi = hex_value(pair[0])?;
        let lo = hex_value(pair[1])?;
        out[i] = (hi << 4) | lo;
    }
    Some(out)
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_dir_resolution_order() {
        let home = std::ffi::OsStr::new("/home/tab");
        let xdg = std::ffi::OsStr::new("/xdg");
        let over = std::ffi::OsStr::new("/override");

        // Override wins, then XDG, then the platform default.
        assert_eq!(
            config_dir_from(Some(over), Some(xdg), Some(home)).unwrap(),
            PathBuf::from("/override")
        );
        assert_eq!(
            config_dir_from(None, Some(xdg), Some(home)).unwrap(),
            PathBuf::from("/xdg/slateway")
        );
        let default = config_dir_from(None, None, Some(home)).unwrap();
        assert!(default.starts_with(home));
        assert!(default.ends_with("slateway"));

        // A relative XDG_CONFIG_HOME violates the XDG base-directory
        // spec and is ignored: the platform default applies.
        let relative = std::ffi::OsStr::new("relative/config");
        let with_relative = config_dir_from(None, Some(relative), Some(home)).unwrap();
        assert!(with_relative.starts_with(home));
        assert!(with_relative.ends_with("slateway"));

        // Empty variables are treated as unset-and-invalid.
        let empty = std::ffi::OsStr::new("");
        assert!(config_dir_from(Some(empty), None, Some(home)).is_err());
        assert!(config_dir_from(None, Some(empty), Some(home)).is_err());
        assert!(config_dir_from(None, None, None).is_err());
    }

    #[test]
    fn host_key_is_created_once_and_reloaded() {
        let dir = tempfile::tempdir().unwrap();
        let first = load_or_create_host_key(dir.path()).unwrap();
        let key_file = dir.path().join("host.key");
        let meta = std::fs::metadata(&key_file).unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(meta.permissions().mode() & 0o777, 0o600);
        }
        let _ = meta;

        let second = load_or_create_host_key(dir.path()).unwrap();
        assert_eq!(first.private, second.private);
    }

    #[test]
    fn host_key_with_wrong_length_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path()).unwrap();
        std::fs::write(dir.path().join("host.key"), [0u8; 31]).unwrap();
        assert!(matches!(
            load_or_create_host_key(dir.path()),
            Err(KeysError::BadKeyLength {
                len: 31,
                expected: 32
            })
        ));
    }

    #[test]
    fn known_tablets_round_trip_and_dedupe() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("known-tablets");
        let mut store = KnownTablets::load(&path).unwrap();
        assert_eq!(store.find(&[1u8; KEY_LEN]), None);

        store
            .insert(KnownTablet {
                public: [0xabu8; KEY_LEN],
                name: "Tab A7".into(),
            })
            .unwrap();
        store
            .insert(KnownTablet {
                public: [0x01u8; KEY_LEN],
                name: "S6 Lite".into(),
            })
            .unwrap();
        // Same key, new name: an update, not a duplicate line.
        store
            .insert(KnownTablet {
                public: [0xabu8; KEY_LEN],
                name: "Tab A7 (renamed)".into(),
            })
            .unwrap();

        let reloaded = KnownTablets::load(&path).unwrap();
        assert_eq!(reloaded.tablets.len(), 2);
        assert_eq!(
            reloaded.find(&[0xabu8; KEY_LEN]).unwrap().name,
            "Tab A7 (renamed)"
        );
        assert_eq!(reloaded.find(&[0x01u8; KEY_LEN]).unwrap().name, "S6 Lite");
    }

    #[test]
    fn known_tablets_skips_malformed_lines() {
        // A malformed line costs its own entry only: the rest of the
        // store stays usable and the skips are reported with their
        // line numbers.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("known-tablets");
        std::fs::write(&path, "not-hex no-key\n\n# comment\naabb no-space-name\n").unwrap();
        let store = KnownTablets::load(&path).unwrap();
        assert!(store.tablets.is_empty());
        let malformed = store.malformed();
        assert_eq!(malformed.len(), 2);
        assert_eq!(malformed[0].0, 1);
        assert_eq!(malformed[1].0, 4);

        // A good line survives alongside the bad one.
        let (private, public) = generate_keypair().unwrap();
        let _ = private;
        let key: [u8; KEY_LEN] = public.as_slice().try_into().unwrap();
        let mut text = hex_encode(&key);
        text.push_str(" good-tablet\nnot-hex bad\n");
        std::fs::write(&path, text).unwrap();
        let store = KnownTablets::load(&path).unwrap();
        assert_eq!(store.tablets.len(), 1);
        assert_eq!(store.tablets[0].name, "good-tablet");
        assert_eq!(store.malformed().len(), 1);
    }

    /// A tablet name from the handshake payload cannot forge registry
    /// lines or bloat the store.
    #[test]
    fn insert_rejects_hostile_names() {
        let dir = tempfile::tempdir().unwrap();
        let mut known = KnownTablets::load(&dir.path().join("known-tablets")).unwrap();
        let (private, public) = generate_keypair().unwrap();
        let _ = private;
        let public: [u8; KEY_LEN] = public.as_slice().try_into().unwrap();

        let newline = KnownTablet {
            public,
            name: "Tab\nforged-line".into(),
        };
        assert!(known.insert(newline).is_err());

        let long = KnownTablet {
            public,
            name: "x".repeat(129),
        };
        assert!(known.insert(long).is_err());

        // A clean name still stores (and is reloadable).
        let ok = KnownTablet {
            public,
            name: "SM-P620".into(),
        };
        known.insert(ok).unwrap();
        let reloaded = KnownTablets::load(&dir.path().join("known-tablets")).unwrap();
        assert_eq!(reloaded.tablets.len(), 1);
        assert_eq!(reloaded.tablets[0].name, "SM-P620");
    }

    #[test]
    fn hex_helpers_round_trip() {
        let bytes: [u8; KEY_LEN] = [
            0x00, 0x0f, 0xa5, 0xff, 0x10, 0x20, 0x30, 0x40, 0x50, 0x60, 0x70, 0x80, 0x90, 0xa0,
            0xb0, 0xc0, 0xd0, 0xe0, 0xf0, 0x01, 0x11, 0x21, 0x31, 0x41, 0x51, 0x61, 0x71, 0x81,
            0x91, 0xa1, 0xb1, 0xc1,
        ];
        let text = hex_encode(&bytes);
        assert_eq!(
            text,
            "000fa5ff102030405060708090a0b0c0d0e0f001112131415161718191a1b1c1"
        );
        assert_eq!(hex_decode(&text).unwrap(), bytes);
        assert_eq!(hex_decode("AABBCCDD_"), None);
        assert_eq!(hex_decode("aabbcc"), None);
    }
}
