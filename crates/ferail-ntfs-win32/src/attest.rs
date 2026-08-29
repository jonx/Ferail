//! Interim integrity attestation for the elevated Fast NTFS helper.
//!
//! Ferail launches `ferail-ntfs-helper.exe` **elevated**, from the directory
//! it was unzipped into — a directory the invoking user can write. Until the
//! Windows package carries an Authenticode signature (see
//! `docs/features/WINDOWS_FAST_NTFS.md`), nothing in the OS distinguishes our
//! helper from a replacement dropped beside it, so this module stands in.
//!
//! What it actually buys, stated precisely because the difference matters:
//!
//! * **A closed check-to-launch window (a real guarantee).** The helper is
//!   opened denying other writers *and deleters*, hashed through that same
//!   handle, and the handle is held across `ShellExecuteExW`. Nothing can
//!   swap the file between the hash and the elevation.
//! * **A reliable catch for the mundane failures.** A stale helper left by an
//!   older version, a half-extracted ZIP, an interrupted update, or a helper
//!   replaced *on its own* all fail closed into the Portable engine.
//! * **Cost, not immunity, against a local attacker.** Someone who can write
//!   the helper can usually write `Ferail.exe` too, and patch the expected
//!   digest out. The salted construction below means they must reverse the
//!   binary rather than search it for a known 32-byte digest and overwrite —
//!   a scripted swap stops working, a determined one does not.
//!
//! Only an Authenticode signature fixes the last point properly, because the
//! signal is then enforced by Windows itself: UAC names the publisher, so a
//! substituted helper prompts as an unknown one where the user can see it.
//! Treat this module as a tripwire and a stopgap, never as that boundary.
//!
//! ## Why the digest is salted
//!
//! The expected value is `SHA-256(salt ‖ file ‖ salt)` with a 32-byte salt
//! drawn fresh for each release build. A plain `SHA-256(file)` would sit in
//! `Ferail.exe` as exactly the 32 bytes an attacker gets by hashing the
//! binary they want to substitute, making the patch a find-and-replace. With
//! the salt, neither constant is derivable from the helper alone.
//!
//! A self-binding scheme (folding the parent's own hash into the expected
//! value, so patching the parent invalidates it) was considered and rejected:
//! Authenticode signing rewrites the parent after the build, which would break
//! the binding at exactly the moment we start signing.

use std::fs::{File, OpenOptions};
use std::io::{self, Read};
use std::path::Path;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::OnceLock;

use sha2::{Digest, Sha256};

/// `FILE_SHARE_READ` — other openers may read, but **not** write, rename or
/// delete. Spelled out here so this module stays free of the `windows` crate
/// and its unit tests keep running on macOS/Linux.
#[cfg(windows)]
const FILE_SHARE_READ: u32 = 0x0000_0001;

/// Read buffer for hashing. The helper is a small binary; this only keeps the
/// hash off the stack and out of a per-byte loop.
const READ_CHUNK: usize = 64 * 1024;

static ATTESTATION: OnceLock<HelperAttestation> = OnceLock::new();

/// Outcome of the most recent [`open_verified`], for diagnostics. 0 = never
/// ran, 1 = attested, 2 = ran without an expected digest.
static LAST_TRUST: AtomicU8 = AtomicU8::new(0);

/// The expected identity of the helper binary, baked into the parent at build
/// time by `scripts/package-win.ps1`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HelperAttestation {
    salt: [u8; 32],
    expected: [u8; 32],
}

impl HelperAttestation {
    /// An attestation no file can satisfy — the salted digest of any input is
    /// a SHA-256 output, and finding one that is all zeros is a preimage
    /// problem. Install this when the expected identity should have been
    /// present but could not be read, so the launch path fails closed into
    /// Portable instead of quietly running unverified.
    pub const UNMATCHABLE: Self = Self {
        salt: [0u8; 32],
        expected: [0u8; 32],
    };

    pub const fn new(salt: [u8; 32], expected: [u8; 32]) -> Self {
        Self { salt, expected }
    }
}

/// Install the expected helper identity. Idempotent: the first call wins and
/// later ones are ignored, so callers may invoke it on every scan rather than
/// depending on boot ordering. Returns whether this call installed the value.
pub fn set_helper_attestation(attestation: HelperAttestation) -> bool {
    ATTESTATION.set(attestation).is_ok()
}

/// Whether this build carries an expected helper identity at all. False in
/// ordinary `cargo build` development trees, where no packaging step ran.
pub fn helper_attestation_configured() -> bool {
    ATTESTATION.get().is_some()
}

/// What the last helper launch could prove, or `None` if none has run yet.
/// Diagnostics use this to report a development build honestly rather than
/// implying a verification that never happened.
pub fn last_helper_trust() -> Option<HelperTrust> {
    match LAST_TRUST.load(Ordering::Relaxed) {
        1 => Some(HelperTrust::Attested),
        2 => Some(HelperTrust::Unattested),
        _ => None,
    }
}

/// How much the launch path could actually prove about the helper it opened.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HelperTrust {
    /// The bytes on disk matched the digest compiled into this build.
    Attested,
    /// This build carries no expected digest — a development tree. The helper
    /// still runs; the caller is expected to say so in diagnostics.
    Unattested,
}

#[derive(Debug)]
#[cfg_attr(not(windows), allow(dead_code))]
pub enum AttestError {
    /// The helper could not be opened or read. A sharing violation lands here
    /// too: something already holds it in a way that forbids our read.
    Unreadable(io::Error),
    /// The helper is present and readable but is not the binary this build
    /// was packaged with.
    Mismatch,
}

impl std::fmt::Display for AttestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unreadable(error) => write!(f, "helper could not be read: {error}"),
            Self::Mismatch => f.write_str("helper does not match this build"),
        }
    }
}

impl std::error::Error for AttestError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Unreadable(error) => Some(error),
            Self::Mismatch => None,
        }
    }
}

/// An opened, verified helper whose file handle is still held.
///
/// Keep this alive until `ShellExecuteExW` has returned. Dropping it earlier
/// reopens the window where the verified bytes can be replaced before Windows
/// maps them.
#[cfg_attr(not(windows), allow(dead_code))]
pub struct HeldHelper {
    /// Held purely for its deny-write/deny-delete lock; never read again.
    _file: File,
}

/// Open `path` denying other writers, verify it against this build's expected
/// digest, and hand back the still-open handle.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn open_verified(path: &Path) -> Result<HeldHelper, AttestError> {
    let mut file = open_denying_writers(path).map_err(AttestError::Unreadable)?;
    let trust = match ATTESTATION.get() {
        None => HelperTrust::Unattested,
        Some(attestation) => {
            let actual =
                digest_reader(&mut file, &attestation.salt).map_err(AttestError::Unreadable)?;
            if actual != attestation.expected {
                return Err(AttestError::Mismatch);
            }
            HelperTrust::Attested
        }
    };
    LAST_TRUST.store(
        match trust {
            HelperTrust::Attested => 1,
            HelperTrust::Unattested => 2,
        },
        Ordering::Relaxed,
    );
    Ok(HeldHelper { _file: file })
}

/// Open for reading while denying writers and deleters, so the bytes cannot
/// change under us between the hash and the launch. On non-Windows hosts the
/// share mode has no equivalent; the function exists there only so the digest
/// logic below stays unit-testable.
#[cfg_attr(not(windows), allow(dead_code))]
fn open_denying_writers(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt as _;
        options.share_mode(FILE_SHARE_READ);
    }
    options.open(path)
}

/// `SHA-256(salt ‖ contents ‖ salt)`, streamed. Bracketing with the salt on
/// both sides keeps the construction distinct from a bare file hash at both
/// ends, so neither a length-extension nor a "hash the candidate and grep"
/// shortcut applies.
fn digest_reader(reader: &mut impl Read, salt: &[u8; 32]) -> io::Result<[u8; 32]> {
    let mut hasher = Sha256::new();
    hasher.update(salt);
    let mut buffer = vec![0u8; READ_CHUNK];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    hasher.update(salt);
    Ok(hasher.finalize().into())
}

/// The digest of an in-memory buffer, for tests and for anything that needs to
/// reproduce what `scripts/package-win.ps1` computes.
pub fn digest_bytes(bytes: &[u8], salt: &[u8; 32]) -> [u8; 32] {
    let mut cursor = bytes;
    digest_reader(&mut cursor, salt).expect("hashing a slice cannot fail")
}

/// Parse 64 hex characters into a 32-byte constant. Used by the generated
/// build constants; returns `None` on any malformed input rather than
/// panicking a release binary into unusability.
pub fn parse_hex32(text: &str) -> Option<[u8; 32]> {
    let bytes = text.as_bytes();
    if bytes.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (index, pair) in bytes.chunks_exact(2).enumerate() {
        let hi = (pair[0] as char).to_digit(16)?;
        let lo = (pair[1] as char).to_digit(16)?;
        out[index] = ((hi << 4) | lo) as u8;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SALT: [u8; 32] = [7u8; 32];

    #[test]
    fn digest_depends_on_contents() {
        assert_ne!(
            digest_bytes(b"helper", &SALT),
            digest_bytes(b"helpeR", &SALT)
        );
    }

    #[test]
    fn digest_depends_on_salt() {
        // The whole point of the salt: the same helper bytes produce a
        // different expected value per build, so the constant in the parent
        // is not derivable from the helper alone.
        let other = [9u8; 32];
        assert_ne!(
            digest_bytes(b"helper", &SALT),
            digest_bytes(b"helper", &other)
        );
    }

    #[test]
    fn digest_is_not_a_bare_file_hash() {
        // A plain SHA-256 of the contents must not equal the salted value,
        // otherwise an attacker could compute the target constant directly.
        let bare: [u8; 32] = Sha256::digest(b"helper").into();
        assert_ne!(digest_bytes(b"helper", &SALT), bare);
    }

    #[test]
    fn streaming_matches_single_shot() {
        // Cross a chunk boundary so the streaming loop is actually exercised.
        let payload = vec![0xABu8; READ_CHUNK * 2 + 17];
        let mut cursor = payload.as_slice();
        let streamed = digest_reader(&mut cursor, &SALT).expect("stream");
        assert_eq!(streamed, digest_bytes(&payload, &SALT));
    }

    #[test]
    fn unmatchable_attestation_matches_nothing() {
        let a = HelperAttestation::UNMATCHABLE;
        for payload in [b"".as_slice(), b"helper", b"\x00\x00\x00\x00"] {
            assert_ne!(digest_bytes(payload, &a.salt), a.expected);
        }
    }

    #[test]
    fn hex_round_trips_and_rejects_junk() {
        let text: String = SALT.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(parse_hex32(&text), Some(SALT));
        assert_eq!(parse_hex32("cafe"), None);
        assert_eq!(parse_hex32(&"z".repeat(64)), None);
    }

    #[test]
    fn unattested_build_opens_without_a_digest() {
        // A development tree installs no attestation, so the helper still
        // launches — but the caller can see that nothing was proven.
        let dir = std::env::temp_dir().join("ferail-attest-unattested");
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("helper.bin");
        std::fs::write(&path, b"anything").expect("write");
        let _held = open_verified(&path).expect("opens");
        // This test binary never calls `set_helper_attestation`, so the
        // global stays empty for the whole process.
        assert_eq!(last_helper_trust(), Some(HelperTrust::Unattested));
        let _ = std::fs::remove_file(&path);
    }
}
