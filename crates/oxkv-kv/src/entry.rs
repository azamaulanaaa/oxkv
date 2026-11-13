//! One versioned key-value entry.

use oxkv_contracts::version::CommitVersion;

/// One live key-value entry at a commit version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// The immutable byte key.
    pub key: Vec<u8>,
    /// The byte value.
    pub value: Vec<u8>,
    /// The commit version that wrote the value.
    pub commit_version: CommitVersion,
}

impl Entry {
    /// Creates an entry.
    #[must_use]
    pub const fn new(key: Vec<u8>, value: Vec<u8>, commit_version: CommitVersion) -> Self {
        Self {
            key,
            value,
            commit_version,
        }
    }

    /// Returns the encoded length of the entry in bytes.
    #[must_use]
    pub fn encoded_len(&self) -> usize {
        8 + self.key.len() + 8 + self.value.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_encoded_length_covers_both_length_prefixes() {
        let entry = Entry::new(vec![b'a'], vec![1, 2, 3], CommitVersion::new(4));
        assert_eq!(entry.encoded_len(), 8 + 1 + 8 + 3);
    }
}
