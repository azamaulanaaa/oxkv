//! Canonical big-endian decoding helpers.

/// A bounds-checked byte decoder.
pub(crate) struct Decoder<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> Decoder<'a> {
    pub(crate) const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, position: 0 }
    }

    pub(crate) fn take(&mut self, length: usize) -> Option<&'a [u8]> {
        let end = self.position.checked_add(length)?;
        let slice = self.bytes.get(self.position..end)?;
        self.position = end;
        Some(slice)
    }

    pub(crate) fn u8(&mut self) -> Option<u8> {
        self.take(1).map(|slice| slice[0])
    }

    pub(crate) fn u16(&mut self) -> Option<u16> {
        let slice = self.take(2)?;
        let mut bytes = [0_u8; 2];
        bytes.copy_from_slice(slice);
        Some(u16::from_be_bytes(bytes))
    }

    pub(crate) fn u64(&mut self) -> Option<u64> {
        let slice = self.take(8)?;
        let mut bytes = [0_u8; 8];
        bytes.copy_from_slice(slice);
        Some(u64::from_be_bytes(bytes))
    }

    pub(crate) fn bytes(&mut self) -> Option<&'a [u8]> {
        let length = self.u64()?;
        let length = usize::try_from(length).ok()?;
        self.take(length)
    }

    pub(crate) const fn is_finished(&self) -> bool {
        self.position == self.bytes.len()
    }

    pub(crate) const fn remaining(&self) -> usize {
        self.bytes.len() - self.position
    }
}
