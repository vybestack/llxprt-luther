//! Hex-encoded SHA-256, in one place.
//!
//! This existed as four byte-identical private copies plus a fifth variant
//! before the tool contract needed a digest as well. A hash is a poor thing to
//! keep several copies of: every caller depends on producing the same string
//! for the same bytes, and nothing checked that the copies agreed.

use sha2::{Digest, Sha256};

/// Hex-encoded SHA-256 of `bytes`, lowercase, 64 characters.
///
/// Some callers use this digest to decide whether captured evidence is
/// authentic, so a subtle formatting difference here would silently reject a
/// genuine capture or accept a fabricated one. Keeping a single implementation
/// means that property is established once.
#[must_use]
pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher
        .finalize()
        .iter()
        // Hex-encoded by indexing rather than by formatting: a discarded
        // formatting Result could yield a short digest that then compares
        // unequal for a reason having nothing to do with the input. Indexing
        // cannot fail, and the width is therefore guaranteed.
        .fold(String::with_capacity(64), |mut hex, byte| {
            const DIGITS: &[u8; 16] = b"0123456789abcdef";
            hex.push(DIGITS[usize::from(byte >> 4)] as char);
            hex.push(DIGITS[usize::from(byte & 0x0f)] as char);
            hex
        })
}

#[cfg(test)]
mod tests {
    use super::sha256_hex;

    /// Every byte value once, so the encoder is exercised over its whole range.
    const ALL_BYTE_VALUES: [u8; 256] = {
        let mut bytes = [0u8; 256];
        let mut index = 0;
        while index < 256 {
            bytes[index] = index as u8;
            index += 1;
        }
        bytes
    };

    /// Published SHA-256 vectors, so the shared implementation is anchored to
    /// values that do not come from this codebase.
    #[test]
    fn it_matches_the_published_vectors() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            sha256_hex(&b"a".repeat(1000)),
            "41edece42d63e8d9bf515a9ba6932e1c20cbc9f5a5d134645adb5db1b9737ea3"
        );
    }

    /// The shared function agrees with the formatting path it replaced.
    ///
    /// Four callers previously formatted the same digest with `{:x}`. Replacing
    /// them is only safe if both encodings produce identical strings, including
    /// for inputs whose digests contain high bytes and leading zero nibbles,
    /// where a hand-rolled encoder is most likely to differ. These values were
    /// computed independently of this codebase.
    #[test]
    fn it_agrees_with_the_formatting_path_it_replaced() {
        let cases: [(&[u8], &str); 3] = [
            (
                &[0u8],
                "6e340b9cffb37a989ca544e6bb780a2c78901d3fb33738768511a30617afa01d",
            ),
            (
                &[0xff; 64],
                "8667e718294e9e0df1d30600ba3eeb201f764aad2dad72748643e4a285e1d1f7",
            ),
            (
                &ALL_BYTE_VALUES,
                "40aff2e9d2d8922e47afd4648e6967497158785fbd1da870e7110266bf944880",
            ),
        ];

        for (input, expected) in cases {
            assert_eq!(sha256_hex(input), expected, "digest for {input:?}");
        }
    }

    /// Input beyond any plausible internal threshold still changes the digest.
    ///
    /// A digest that silently stopped reading at some size would return the
    /// same value for two different inputs, so a caller comparing captured
    /// evidence would accept a file whose tail had been rewritten. The two
    /// inputs here differ only after the first megabyte.
    #[test]
    fn it_does_not_stop_reading_at_a_size_threshold() {
        let mut base = vec![b'a'; 1_000_000];
        base.push(b'x');
        let mut altered = vec![b'a'; 1_000_000];
        altered.push(b'y');

        assert_ne!(
            sha256_hex(&base),
            sha256_hex(&altered),
            "inputs differing only past one megabyte must not share a digest"
        );
    }

    /// Every digest is exactly 64 lowercase hex characters.
    ///
    /// The formatting path is the one that could silently shorten a digest,
    /// which would compare unequal for a reason unrelated to the input.
    #[test]
    fn every_digest_is_full_width_lowercase_hex() {
        for input in [b"".as_slice(), b"abc", b"\x00\x01\x02", &[0xff; 64]] {
            let digest = sha256_hex(input);
            assert_eq!(digest.len(), 64, "digest for {input:?} was not 64 chars");
            assert!(
                digest
                    .chars()
                    .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c)),
                "digest for {input:?} was not lowercase hex: {digest}"
            );
        }
    }
}

pub mod recovery_epoch;
