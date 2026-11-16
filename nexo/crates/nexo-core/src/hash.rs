//! Hex and SHA-256 helpers the crates share, so each has one copy.

use sha2::{Digest, Sha256};

/// Lower-case hex of `bytes`, two digits a byte.
pub fn hex_lower(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The SHA-256 of `bytes` as 64 lower-case hex digits.
pub fn sha256_hex(bytes: &[u8]) -> String {
    hex_lower(&Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_of_abc_is_the_known_digest() {
        assert_eq!(sha256_hex(b"abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }

    #[test]
    fn hex_keeps_leading_zeros() {
        assert_eq!(hex_lower(&[0, 1, 255]), "0001ff");
        assert_eq!(hex_lower(&[]), "");
    }
}
