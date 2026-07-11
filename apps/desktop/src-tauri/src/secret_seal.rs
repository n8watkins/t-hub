//! At-rest sealing of T-Hub's secret material (item-3 Pillar B, RATIFIED
//! 2026-07-10, general-decision #5: DPAPI as THE mechanism on the Windows host).
//!
//! T-Hub's secrets - the full control token (`server-key`), the read token
//! (`server-read-key`), and each per-session identity secret in `identities.json` -
//! live on the user's home, which on the real deployment is the WINDOWS home
//! (`control.json` is symlinked into `~/.t-hub` for WSL). This module seals those
//! payloads before they touch disk.
//!
//! ## Mechanism, honestly graded
//! - **Windows (the real host): DPAPI.** `CryptProtectData` / `CryptUnprotectData`
//!   with `CRYPTPROTECT_UI_FORBIDDEN` and NO `CRYPTPROTECT_LOCALMACHINE`, so the
//!   blob is sealed under the current Windows USER's data-protection key and can only
//!   be unsealed by that same user on that same machine. This defends casual /
//!   other-principal reads of the shared-readable state files. Graded MEDIUM (N2): it
//!   is NOT a wall against a same-Windows-user root-equivalent extraction (the true
//!   isolation boundary is a separate OS user / container, general-decision #4).
//! - **Non-Windows (pure-WSL / ext4, and the Linux dev+CI build): plaintext behind
//!   the `0600` mode bits the callers already set.** Real on ext4, COSMETIC on the
//!   NTFS/drvfs state path (MED-7) - which is exactly why DPAPI is primary on the
//!   real host, and the `0600` fallback is scoped to a pure-WSL(ext4) deployment.
//!
//! The on-disk format is self-describing and backward-compatible: a sealed value is
//! `thub-sealed:v1:<base64(ciphertext)>`; anything without that prefix is read as
//! legacy/fallback PLAINTEXT, so an upgrade transparently reads pre-item-3 key files
//! and re-seals them on the next write.

// base64 is only used on the DPAPI (Windows) path; gating the import keeps the Linux
// dev/CI build warning-clean.
#[cfg(windows)]
use base64::{engine::general_purpose::STANDARD, Engine as _};

/// Marks a DPAPI-sealed, base64-encoded on-disk value. A stored string lacking this
/// prefix is a legacy/fallback plaintext secret.
const SEAL_PREFIX: &str = "thub-sealed:v1:";

/// Whether at-rest DPAPI sealing is active on this build/host. `false` on the Linux
/// dev+CI build and on a pure-WSL deployment (where the `0600` fallback stands in).
pub fn sealing_active() -> bool {
    cfg!(windows)
}

/// Seal a secret string for at-rest storage, returning the exact bytes-as-string to
/// write to disk. On Windows the payload is DPAPI-sealed, base64-encoded, and
/// prefixed; elsewhere it is returned verbatim (the `0600` fallback). A DPAPI failure
/// falls back to storing the plaintext (best-effort - availability over
/// confidentiality, matching the existing best-effort `0600` posture) with a warning.
pub fn seal_str(plain: &str) -> String {
    #[cfg(windows)]
    {
        match dpapi_protect(plain.as_bytes()) {
            Some(cipher) => format!("{SEAL_PREFIX}{}", STANDARD.encode(cipher)),
            None => {
                eprintln!(
                    "t-hub-seal: DPAPI protect failed; storing the secret UNSEALED \
                     (0600 fallback). Confidentiality at rest is reduced until the next \
                     successful seal."
                );
                plain.to_string()
            }
        }
    }
    #[cfg(not(windows))]
    {
        plain.to_string()
    }
}

/// Unseal a value previously written by [`seal_str`]. Accepts BOTH a sealed value
/// (prefix present) and a legacy/fallback PLAINTEXT value (no prefix), so an upgrade
/// reads old files and the fallback host round-trips. Returns `None` only when a
/// SEALED blob cannot be opened (wrong Windows user/machine, corruption, or a sealed
/// blob encountered on a non-DPAPI host).
pub fn unseal_str(stored: &str) -> Option<String> {
    let trimmed = stored.trim();
    if let Some(b64) = trimmed.strip_prefix(SEAL_PREFIX) {
        #[cfg(windows)]
        {
            let cipher = STANDARD.decode(b64).ok()?;
            let plain = dpapi_unprotect(&cipher)?;
            return String::from_utf8(plain).ok();
        }
        #[cfg(not(windows))]
        {
            // A DPAPI-sealed blob cannot be opened without DPAPI (wrong platform).
            let _ = b64;
            eprintln!(
                "t-hub-seal: found a DPAPI-sealed secret on a non-Windows host; cannot \
                 unseal it here (this store belongs to the Windows-hosted app)."
            );
            return None;
        }
    }
    // No prefix: legacy pre-item-3 or fallback plaintext.
    Some(trimmed.to_string())
}

/// True iff `stored` is in the sealed on-disk form (vs legacy/fallback plaintext).
/// Callers use this to decide whether a re-write is needed to upgrade a plaintext
/// key file to the sealed form on a DPAPI host.
pub fn is_sealed(stored: &str) -> bool {
    stored.trim_start().starts_with(SEAL_PREFIX)
}

#[cfg(windows)]
fn dpapi_protect(plain: &[u8]) -> Option<Vec<u8>> {
    use windows::Win32::Foundation::{LocalFree, HLOCAL};
    use windows::Win32::Security::Cryptography::{
        CryptProtectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
    };
    unsafe {
        let input = CRYPT_INTEGER_BLOB {
            cbData: plain.len() as u32,
            pbData: plain.as_ptr() as *mut u8,
        };
        let mut output = CRYPT_INTEGER_BLOB::default();
        // No entropy, no prompt, no LOCALMACHINE => sealed under the current USER key.
        CryptProtectData(
            &input,
            None,
            None,
            None,
            None,
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
        .ok()?;
        let bytes = std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec();
        // The output buffer is owned by the API; free it via LocalFree.
        let _ = LocalFree(Some(HLOCAL(output.pbData as *mut _)));
        Some(bytes)
    }
}

#[cfg(windows)]
fn dpapi_unprotect(cipher: &[u8]) -> Option<Vec<u8>> {
    use windows::Win32::Foundation::{LocalFree, HLOCAL};
    use windows::Win32::Security::Cryptography::{
        CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
    };
    unsafe {
        let input = CRYPT_INTEGER_BLOB {
            cbData: cipher.len() as u32,
            pbData: cipher.as_ptr() as *mut u8,
        };
        let mut output = CRYPT_INTEGER_BLOB::default();
        CryptUnprotectData(
            &input,
            None,
            None,
            None,
            None,
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
        .ok()?;
        let bytes = std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec();
        let _ = LocalFree(Some(HLOCAL(output.pbData as *mut _)));
        Some(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_plaintext_round_trips_and_is_not_marked_sealed() {
        // A pre-item-3 key file (raw token, no prefix) reads back verbatim on every
        // platform, so an upgrade never loses the paired credential.
        let stored = "b1c2d3-legacy-token";
        assert!(!is_sealed(stored));
        assert_eq!(unseal_str(stored).as_deref(), Some("b1c2d3-legacy-token"));
    }

    #[test]
    fn seal_round_trips_on_this_host() {
        // On the Linux dev/CI build seal_str is the identity (0600 fallback), so the
        // round-trip holds without DPAPI; on Windows this exercises the real DPAPI
        // seal/unseal. Either way seal->unseal must recover the exact secret.
        let secret = "example-session-secret-value";
        let stored = seal_str(secret);
        assert_eq!(unseal_str(&stored).as_deref(), Some(secret));
        // The seal marker is present iff sealing is active on this host.
        assert_eq!(is_sealed(&stored), sealing_active());
    }

    #[test]
    fn whitespace_is_tolerated() {
        // Key files are written without a trailing newline, but a hand-edit might add
        // one; unseal must tolerate surrounding whitespace like the old trim() read.
        assert_eq!(unseal_str("  plain-token \n").as_deref(), Some("plain-token"));
    }
}
