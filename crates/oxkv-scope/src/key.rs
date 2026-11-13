//! Key form checks for a user scope.
//!
//! A v1 system key has the form
//! `0x01 || table-name || 0x00 || 0xFF || system-id || ...`.
//! A user scope admits user keys only.

use oxkv_contracts::keyspace::{KEY_ENCODING_VERSION, SYSTEM_MARKER};

/// Returns the system identifier of a v1 key.
///
/// The function reads the key encoding version, the table terminator, and the
/// system marker. It returns `None` for a user key or a malformed key.
pub(crate) fn system_id(key: &[u8]) -> Option<u8> {
    if key.first().copied() != Some(KEY_ENCODING_VERSION) {
        return None;
    }
    let rest = key.get(1..)?;
    let terminator = rest.iter().position(|byte| *byte == 0x00)?;
    let marker = terminator.checked_add(2)?;
    if key.get(marker).copied() != Some(SYSTEM_MARKER) {
        return None;
    }
    key.get(marker.checked_add(1)?).copied()
}

/// Returns true when the key is a v1 system key.
pub(crate) fn is_system_key(key: &[u8]) -> bool {
    system_id(key).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_user_key_is_not_a_system_key() {
        assert!(!is_system_key(b"\x01accounts\x00\x10alice\x00\x00"));
        assert_eq!(system_id(b"\x01accounts\x00\x10alice\x00\x00"), None);
    }

    #[test]
    fn a_real_system_key_is_detected() {
        assert!(is_system_key(
            b"\x01accounts\x00\xff\x01\x10receipt\x00\x00"
        ));
        assert_eq!(
            system_id(b"\x01accounts\x00\xff\x01\x10receipt\x00\x00"),
            Some(0x01)
        );
    }

    #[test]
    fn a_malformed_key_is_not_a_system_key() {
        assert!(!is_system_key(b"not a key"));
        assert!(!is_system_key(b"\x01accounts"));
        assert!(!is_system_key(b"\x02accounts\x00\xff\x01"));
    }
}
