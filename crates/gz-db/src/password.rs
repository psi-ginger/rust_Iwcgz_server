//! Password hashing / verification for `accounts.password_hash`.
//!
//! Development format: `sha256:<64 lowercase hex>`, produced by the seed with
//! pgcrypto's `sha256()` (`crates/gz-db/sql/0002_seed_dev.sql`). This module
//! mirrors that so the login path can verify it.
//!
//! This is deliberately the weakest possible hash and is **development only**:
//! a memory-hard KDF (argon2) is separate hardening work. The `sha256:` prefix
//! is what makes that upgrade a data migration rather than a code change.

use sha2::{Digest, Sha256};

/// Prefix identifying the development hash format.
pub const SHA256_PREFIX: &str = "sha256:";

const HEX: &[u8; 16] = b"0123456789abcdef";

/// Hashes `password` into the development `sha256:<hex>` format.
#[must_use]
pub fn hash_password(password: &str) -> String {
    let digest = Sha256::digest(password.as_bytes());
    let mut out = String::with_capacity(SHA256_PREFIX.len() + 64);
    out.push_str(SHA256_PREFIX);
    for byte in digest {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

/// Verifies `password` against a stored `sha256:<hex>` hash.
///
/// The digest comparison folds every byte into one accumulator, so it does not
/// return early on the first mismatch. Returns `false` for any other format,
/// including an unknown prefix or wrong-length hex.
#[must_use]
pub fn verify_password(stored: &str, password: &str) -> bool {
    let Some(hex_digest) = stored.strip_prefix(SHA256_PREFIX) else {
        return false;
    };
    if hex_digest.len() != 64 {
        return false;
    }

    let digest = Sha256::digest(password.as_bytes());
    let mut diff = 0u8;
    for (i, byte) in digest.iter().enumerate() {
        let hi = hex_value(hex_digest.as_bytes()[i * 2]);
        let lo = hex_value(hex_digest.as_bytes()[i * 2 + 1]);
        diff |= byte ^ ((hi << 4) | lo);
    }
    diff == 0
}

fn hex_value(c: u8) -> u8 {
    match c {
        b'0'..=b'9' => c - b'0',
        b'a'..=b'f' => c - b'a' + 10,
        b'A'..=b'F' => c - b'A' + 10,
        _ => 0xff,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_matches_the_pgcrypto_seed_value() {
        // This exact literal is what the seed's `encode(sha256('test'), 'hex')`
        // produced against PostgreSQL 18.6; the round-trip also proves it.
        assert_eq!(
            hash_password("test"),
            "sha256:9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08"
        );
        assert!(verify_password(&hash_password("test"), "test"));
    }

    #[test]
    fn rejects_wrong_password_and_bad_formats() {
        let stored = hash_password("test");
        assert!(!verify_password(&stored, "Test"));
        assert!(!verify_password(&stored, ""));
        assert!(!verify_password("plaintext", "plaintext"));
        assert!(!verify_password("sha256:tooshort", "test"));
        assert!(!verify_password("sha256:", "test"));
    }

    #[test]
    fn accepts_uppercase_hex_but_the_writer_emits_lowercase() {
        let upper = hash_password("test")
            .to_uppercase()
            .replacen("SHA256:", "sha256:", 1);
        assert!(verify_password(&upper, "test"));
    }
}
