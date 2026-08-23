//! Small utilities shared by module binaries.

use sha2::{Digest, Sha256};

/// Lowercase hex sha256 of `bytes` — the change-detection / checksum
/// convention used across cliphistory modules and core.
pub fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    #[test]
    fn known_vector() {
        assert_eq!(
            super::sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
