//! One operation in a batch.

/// One mutation in a batch.
///
/// The batch applies operations in slice order. A later operation on the same
/// key replaces the effect of an earlier operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Operation {
    /// Store a value for a key.
    Put {
        /// The key.
        key: Vec<u8>,
        /// The value.
        value: Vec<u8>,
    },
    /// Remove the live value of a key.
    Delete {
        /// The key.
        key: Vec<u8>,
    },
}

/// The canonical tag of a put operation.
pub(crate) const OP_PUT: u8 = 1;
/// The canonical tag of a delete operation.
pub(crate) const OP_DELETE: u8 = 2;

impl Operation {
    /// Creates a put operation.
    #[must_use]
    pub const fn put(key: Vec<u8>, value: Vec<u8>) -> Self {
        Self::Put { key, value }
    }

    /// Creates a delete operation.
    #[must_use]
    pub const fn delete(key: Vec<u8>) -> Self {
        Self::Delete { key }
    }

    /// Returns the key of the operation.
    #[must_use]
    pub fn key(&self) -> &[u8] {
        match self {
            Self::Put { key, .. } | Self::Delete { key } => key,
        }
    }

    /// Returns the value of a put operation.
    #[must_use]
    pub fn value(&self) -> Option<&[u8]> {
        match self {
            Self::Put { value, .. } => Some(value),
            Self::Delete { .. } => None,
        }
    }

    /// Returns the encoded length of the operation in bytes.
    #[must_use]
    pub fn encoded_len(&self) -> usize {
        match self {
            Self::Put { key, value } => 1 + 8 + key.len() + 8 + value.len(),
            Self::Delete { key } => 1 + 8 + key.len(),
        }
    }

    pub(crate) fn encode_into(&self, bytes: &mut Vec<u8>) {
        match self {
            Self::Put { key, value } => {
                bytes.push(OP_PUT);
                push_bytes(bytes, key);
                push_bytes(bytes, value);
            }
            Self::Delete { key } => {
                bytes.push(OP_DELETE);
                push_bytes(bytes, key);
            }
        }
    }
}

pub(crate) fn push_bytes(bytes: &mut Vec<u8>, value: &[u8]) {
    bytes.extend_from_slice(&u64::try_from(value.len()).unwrap_or(u64::MAX).to_be_bytes());
    bytes.extend_from_slice(value);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operations_expose_their_key_and_value() {
        let put = Operation::put(vec![b'a'], vec![1]);
        assert_eq!(put.key(), b"a");
        assert_eq!(put.value(), Some(&[1][..]));

        let delete = Operation::delete(vec![b'b']);
        assert_eq!(delete.key(), b"b");
        assert_eq!(delete.value(), None);
    }
}
