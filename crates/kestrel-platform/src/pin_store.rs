//! Encrypted, atomic storage for pinned clipboard text.
//!
//! The file is a magic and version header, a fresh nonce, and the
//! XChaCha20-Poly1305 ciphertext of length-prefixed records. Plaintext buffers are
//! zeroized after use, and any truncated, corrupt, re-keyed, or unknown
//! version is reported without exposing partial content.

use std::{
    fmt,
    fs::{self, File, OpenOptions},
    io::{self, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use chacha20poly1305::{
    XChaCha20Poly1305, XNonce,
    aead::{Aead, KeyInit, Payload},
};
use zeroize::Zeroizing;

use crate::secret_service::PinKey;

/// File name inside the application's data directory.
pub const PIN_FILE_NAME: &str = "clipboard-pins.bin";

const MAGIC: &[u8; 8] = b"KSTRLPIN";
const FORMAT_VERSION: u8 = 1;
const HEADER_LEN: usize = MAGIC.len() + 1;
const NONCE_LEN: usize = 24;
const TAG_LEN: usize = 16;
/// Refused before reading, so a planted file cannot exhaust memory.
const MAX_FILE_BYTES: u64 = 128 * 1024 * 1024 + 64 * 1024;
const FILE_MODE: u32 = 0o600;
const DIRECTORY_MODE: u32 = 0o700;
const TEMPORARY_ATTEMPTS: u32 = 16;
static TEMPORARY_SERIAL: AtomicU64 = AtomicU64::new(0);

/// One pinned text entry as stored.
pub struct SavedPin {
    text: Zeroizing<String>,
    pinned_at_unix: u64,
}

impl SavedPin {
    pub fn new(text: impl Into<String>, pinned_at_unix: u64) -> Self {
        Self {
            text: Zeroizing::new(text.into()),
            pinned_at_unix,
        }
    }

    /// Wraps text that is already in a wiping buffer, without copying it.
    pub fn from_zeroizing(text: Zeroizing<String>, pinned_at_unix: u64) -> Self {
        Self {
            text,
            pinned_at_unix,
        }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn pinned_at_unix(&self) -> u64 {
        self.pinned_at_unix
    }

    /// Takes the text for a new in-memory entry.
    pub fn into_text(self) -> Zeroizing<String> {
        self.text
    }
}

impl fmt::Debug for SavedPin {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SavedPin(..)")
    }
}

// Plaintext payload: a u32 record count, then per record a u64 pin time, a
// u32 UTF-8 byte length, and the bytes; all integers little-endian. Lengths
// are exact, so encoding and decoding never reallocate a buffer holding text.
const COUNT_LEN: usize = 4;
const RECORD_HEADER_LEN: usize = 8 + 4;

/// Why saved pins could not be read or written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinStoreError {
    /// The file is truncated, corrupt, or encrypted with another key.
    Unreadable,
    /// The file carries a format version this build does not know.
    UnsupportedVersion,
    /// The operating system's random source failed.
    Entropy,
    /// The file or its directory could not be read or written.
    Io,
}

impl fmt::Display for PinStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Unreadable => "the saved pins file is unreadable or encrypted with another key",
            Self::UnsupportedVersion => "the saved pins file has an unknown format version",
            Self::Entropy => "the operating system's random source is unavailable",
            Self::Io => "the saved pins file could not be accessed",
        })
    }
}

impl std::error::Error for PinStoreError {}

/// Reads saved pins; a missing file is `None`.
pub fn read_pins(path: &Path, key: &PinKey) -> Result<Option<Vec<SavedPin>>, PinStoreError> {
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(PinStoreError::Io),
    };
    if metadata.len() > MAX_FILE_BYTES {
        return Err(PinStoreError::Unreadable);
    }
    let bytes = fs::read(path).map_err(|_| PinStoreError::Io)?;
    decode(key, &bytes).map(Some)
}

/// Encrypts and atomically replaces the file; returns its byte length.
pub fn write_pins(path: &Path, key: &PinKey, pins: &[SavedPin]) -> Result<usize, PinStoreError> {
    let encoded = encode(key, pins)?;
    write_private_atomic(path, &encoded).map_err(|_| PinStoreError::Io)?;
    Ok(encoded.len())
}

/// Removes the file; returns whether it existed.
pub fn delete_pins(path: &Path) -> Result<bool, PinStoreError> {
    let existed = match fs::remove_file(path) {
        Ok(()) => true,
        Err(error) if error.kind() == io::ErrorKind::NotFound => false,
        Err(_) => return Err(PinStoreError::Io),
    };
    if let Some(parent) = path.parent() {
        if let Ok(directory) = File::open(parent) {
            let _ = directory.sync_all();
        }
    }
    if path.exists() {
        return Err(PinStoreError::Io);
    }
    Ok(existed)
}

/// Whether the file exists.
pub fn pins_file_present(path: &Path) -> bool {
    path.exists()
}

/// Encrypts `pins` into complete file contents.
fn encode(key: &PinKey, pins: &[SavedPin]) -> Result<Zeroizing<Vec<u8>>, PinStoreError> {
    let count = u32::try_from(pins.len()).map_err(|_| PinStoreError::Unreadable)?;
    let capacity = pins.iter().try_fold(COUNT_LEN, |total, pin| {
        total
            .checked_add(RECORD_HEADER_LEN)?
            .checked_add(pin.text().len())
    });
    let mut plaintext = Zeroizing::new(Vec::with_capacity(
        capacity.ok_or(PinStoreError::Unreadable)?,
    ));
    plaintext.extend_from_slice(&count.to_le_bytes());
    for pin in pins {
        let length = u32::try_from(pin.text().len()).map_err(|_| PinStoreError::Unreadable)?;
        plaintext.extend_from_slice(&pin.pinned_at_unix.to_le_bytes());
        plaintext.extend_from_slice(&length.to_le_bytes());
        plaintext.extend_from_slice(pin.text().as_bytes());
    }
    let mut header = [0u8; HEADER_LEN];
    header[..MAGIC.len()].copy_from_slice(MAGIC);
    header[MAGIC.len()] = FORMAT_VERSION;
    let mut nonce = [0u8; NONCE_LEN];
    getrandom::fill(&mut nonce).map_err(|_| PinStoreError::Entropy)?;
    let cipher =
        XChaCha20Poly1305::new_from_slice(key.expose()).map_err(|_| PinStoreError::Entropy)?;
    let ciphertext = cipher
        .encrypt(
            XNonce::from_slice(&nonce),
            Payload {
                msg: &plaintext,
                aad: &header,
            },
        )
        .map_err(|_| PinStoreError::Entropy)?;
    let mut encoded = Zeroizing::new(Vec::with_capacity(
        HEADER_LEN + NONCE_LEN + ciphertext.len(),
    ));
    encoded.extend_from_slice(&header);
    encoded.extend_from_slice(&nonce);
    encoded.extend_from_slice(&ciphertext);
    Ok(encoded)
}

/// Decrypts and decodes complete file contents.
fn decode(key: &PinKey, bytes: &[u8]) -> Result<Vec<SavedPin>, PinStoreError> {
    if bytes.len() < HEADER_LEN + NONCE_LEN + TAG_LEN {
        return Err(PinStoreError::Unreadable);
    }
    let (header, rest) = bytes.split_at(HEADER_LEN);
    if &header[..MAGIC.len()] != MAGIC {
        return Err(PinStoreError::Unreadable);
    }
    if header[MAGIC.len()] != FORMAT_VERSION {
        return Err(PinStoreError::UnsupportedVersion);
    }
    let (nonce, ciphertext) = rest.split_at(NONCE_LEN);
    let cipher =
        XChaCha20Poly1305::new_from_slice(key.expose()).map_err(|_| PinStoreError::Unreadable)?;
    let plaintext = Zeroizing::new(
        cipher
            .decrypt(
                XNonce::from_slice(nonce),
                Payload {
                    msg: ciphertext,
                    aad: header,
                },
            )
            .map_err(|_| PinStoreError::Unreadable)?,
    );
    decode_records(&plaintext)
}

/// Parses the plaintext payload; any inconsistency is `Unreadable`.
fn decode_records(plaintext: &[u8]) -> Result<Vec<SavedPin>, PinStoreError> {
    let (count, mut rest) = plaintext
        .split_first_chunk::<COUNT_LEN>()
        .ok_or(PinStoreError::Unreadable)?;
    let count = u32::from_le_bytes(*count) as usize;
    // Each record needs at least its header, which bounds the allocation.
    if count > rest.len() / RECORD_HEADER_LEN {
        return Err(PinStoreError::Unreadable);
    }
    let mut pins = Vec::with_capacity(count);
    for _ in 0..count {
        let (pinned_at, after_time) = rest
            .split_first_chunk::<8>()
            .ok_or(PinStoreError::Unreadable)?;
        let (length, after_length) = after_time
            .split_first_chunk::<4>()
            .ok_or(PinStoreError::Unreadable)?;
        let length = u32::from_le_bytes(*length) as usize;
        if length > after_length.len() {
            return Err(PinStoreError::Unreadable);
        }
        let (bytes, remaining) = after_length.split_at(length);
        let text = std::str::from_utf8(bytes).map_err(|_| PinStoreError::Unreadable)?;
        let mut owned = Zeroizing::new(String::with_capacity(length));
        owned.push_str(text);
        pins.push(SavedPin::from_zeroizing(
            owned,
            u64::from_le_bytes(*pinned_at),
        ));
        rest = remaining;
    }
    if !rest.is_empty() {
        return Err(PinStoreError::Unreadable);
    }
    Ok(pins)
}

fn write_private_atomic(path: &Path, contents: &[u8]) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "the path has no parent"))?;
    let file_name = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "the path has no file name"))?;
    fs::create_dir_all(parent)?;
    let _ = fs::set_permissions(parent, fs::Permissions::from_mode(DIRECTORY_MODE));

    let (temporary, mut file) = create_temporary(parent, file_name)?;
    let written = file.write_all(contents).and_then(|()| file.sync_all());
    drop(file);
    if let Err(error) = written.and_then(|()| fs::rename(&temporary, path)) {
        let _ = fs::remove_file(&temporary);
        return Err(error);
    }
    let _ = fs::set_permissions(path, fs::Permissions::from_mode(FILE_MODE));
    if let Ok(directory) = File::open(parent) {
        let _ = directory.sync_all();
    }
    Ok(())
}

fn create_temporary(parent: &Path, file_name: &std::ffi::OsStr) -> io::Result<(PathBuf, File)> {
    for _ in 0..TEMPORARY_ATTEMPTS {
        let serial = TEMPORARY_SERIAL.fetch_add(1, Ordering::Relaxed);
        let mut name = std::ffi::OsString::from(".");
        name.push(file_name);
        name.push(format!(".{}.{serial}.tmp", std::process::id()));
        let temporary = parent.join(name);
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(FILE_MODE)
            .open(&temporary)
        {
            Ok(file) => return Ok((temporary, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "no temporary name was available",
    ))
}

#[cfg(test)]
mod tests {
    use std::{fs, os::unix::fs::PermissionsExt};

    use super::{
        PinStoreError, SavedPin, decode, decode_records, delete_pins, encode, pins_file_present,
        read_pins, write_pins,
    };
    use crate::secret_service::{PIN_KEY_BYTES, PinKey};

    fn key(byte: u8) -> PinKey {
        PinKey::from_bytes(&[byte; PIN_KEY_BYTES]).expect("exact length")
    }

    fn pins() -> Vec<SavedPin> {
        vec![
            SavedPin::new("first secret", 1_700_000_000),
            SavedPin::new("second value", 1_700_000_100),
            SavedPin::new(
                "\"quoted\" \\ back\nline\ttab\u{0}nul\u{1b}[0m é 🦅",
                1_700_000_200,
            ),
            SavedPin::new("", u64::MAX),
        ]
    }

    #[test]
    fn encoded_pins_round_trip_and_leave_no_plaintext() {
        let key = key(3);
        let encoded = encode(&key, &pins()).expect("encryption succeeds");
        assert!(encoded.starts_with(b"KSTRLPIN"));
        assert!(
            !encoded
                .windows(b"first secret".len())
                .any(|window| window == b"first secret")
        );
        let decoded = decode(&key, &encoded).expect("decryption succeeds");
        let original = pins();
        assert_eq!(decoded.len(), original.len());
        for (decoded, original) in decoded.iter().zip(&original) {
            assert_eq!(decoded.text(), original.text());
            assert_eq!(decoded.pinned_at_unix(), original.pinned_at_unix());
        }
        assert_eq!(format!("{:?}", decoded[0]), "SavedPin(..)");
    }

    #[test]
    fn a_wrong_key_truncated_file_or_unknown_version_is_unreadable() {
        let encoded = encode(&key(3), &pins()).expect("encryption succeeds");
        assert_eq!(
            decode(&key(4), &encoded).err(),
            Some(PinStoreError::Unreadable)
        );
        for length in [0, 8, 9, 32] {
            assert_eq!(
                decode(&key(3), &encoded[..length.min(encoded.len())]).err(),
                Some(PinStoreError::Unreadable),
                "{length}"
            );
        }
        let mut unknown = encoded.clone();
        unknown[8] = 9;
        assert_eq!(
            decode(&key(3), &unknown).err(),
            Some(PinStoreError::UnsupportedVersion)
        );

        let record = |time: u64, length: u32, bytes: &[u8]| {
            let mut record = time.to_le_bytes().to_vec();
            record.extend_from_slice(&length.to_le_bytes());
            record.extend_from_slice(bytes);
            record
        };
        let payload = |count: u32, records: &[Vec<u8>]| {
            let mut payload = count.to_le_bytes().to_vec();
            for record in records {
                payload.extend_from_slice(record);
            }
            payload
        };
        assert_eq!(
            decode_records(&payload(0, &[])).map(|pins| pins.len()),
            Ok(0)
        );
        for (label, malformed) in [
            ("no count", Vec::new()),
            ("count beyond the payload", payload(u32::MAX, &[])),
            ("missing record", payload(2, &[record(1, 2, b"ok")])),
            ("length past the end", payload(1, &[record(1, 9, b"short")])),
            ("invalid UTF-8", payload(1, &[record(1, 2, &[0xff, 0xfe])])),
            ("trailing bytes", {
                let mut extra = payload(1, &[record(1, 2, b"ok")]);
                extra.push(0);
                extra
            }),
        ] {
            assert_eq!(
                decode_records(&malformed).err(),
                Some(PinStoreError::Unreadable),
                "{label}"
            );
        }
    }

    #[test]
    fn writes_are_private_atomic_and_deletable() {
        let directory = tempfile::TempDir::new().expect("temporary directory");
        let path = directory.path().join(super::PIN_FILE_NAME);
        let written = write_pins(&path, &key(5), &pins()).expect("write succeeds");
        assert!(written > 0);
        assert!(pins_file_present(&path));
        assert_eq!(
            fs::metadata(&path)
                .expect("file metadata")
                .permissions()
                .mode()
                & 0o777,
            super::FILE_MODE
        );
        let parent_mode = fs::metadata(directory.path())
            .expect("directory metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(parent_mode, super::DIRECTORY_MODE);

        let loaded = read_pins(&path, &key(5))
            .expect("read succeeds")
            .expect("file exists");
        assert_eq!(loaded[0].text(), "first secret");

        assert_eq!(delete_pins(&path), Ok(true));
        assert!(!pins_file_present(&path));
        assert_eq!(delete_pins(&path), Ok(false));
        assert!(read_pins(&path, &key(5)).expect("read succeeds").is_none());
    }

    #[test]
    fn no_temporary_files_remain_after_a_write() {
        let directory = tempfile::TempDir::new().expect("temporary directory");
        let path = directory.path().join(super::PIN_FILE_NAME);
        write_pins(&path, &key(6), &pins()).expect("write succeeds");
        let names = fs::read_dir(directory.path())
            .expect("directory lists")
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(names, vec![super::PIN_FILE_NAME.to_owned()]);
    }
}
