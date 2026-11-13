//! Canonical big-endian encoding helpers.
//!
//! Every artifact uses the same primitive layout. Integers are big-endian. A
//! byte string or text is a `u64` length followed by the bytes. The layout has
//! no padding and no platform-dependent part.

use crate::error::IndexError;

pub(crate) fn u8(output: &mut Vec<u8>, value: u8) {
    output.push(value);
}

pub(crate) fn u16(output: &mut Vec<u8>, value: u16) {
    output.extend_from_slice(&value.to_be_bytes());
}

pub(crate) fn u32(output: &mut Vec<u8>, value: u32) {
    output.extend_from_slice(&value.to_be_bytes());
}

pub(crate) fn u64(output: &mut Vec<u8>, value: u64) {
    output.extend_from_slice(&value.to_be_bytes());
}

pub(crate) fn bytes(output: &mut Vec<u8>, value: &[u8]) {
    let length = u64::try_from(value.len()).unwrap_or(u64::MAX);
    u64(output, length);
    output.extend_from_slice(value);
}

pub(crate) fn text(output: &mut Vec<u8>, value: &str) {
    bytes(output, value.as_bytes());
}

pub(crate) fn bool_byte(output: &mut Vec<u8>, value: bool) {
    u8(output, u8::from(value));
}

pub(crate) struct Reader<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> Reader<'a> {
    pub(crate) const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, position: 0 }
    }

    pub(crate) fn finish(&self) -> Result<(), IndexError> {
        if self.position == self.bytes.len() {
            Ok(())
        } else {
            Err(IndexError::IndexMismatch)
        }
    }

    pub(crate) fn take(&mut self, length: usize) -> Result<&'a [u8], IndexError> {
        let end = self
            .position
            .checked_add(length)
            .ok_or(IndexError::IndexMismatch)?;
        let slice = self
            .bytes
            .get(self.position..end)
            .ok_or(IndexError::IndexMismatch)?;
        self.position = end;
        Ok(slice)
    }

    pub(crate) fn u16(&mut self) -> Result<u16, IndexError> {
        let slice = self.take(2)?;
        let mut value = [0_u8; 2];
        value.copy_from_slice(slice);
        Ok(u16::from_be_bytes(value))
    }

    pub(crate) fn u32(&mut self) -> Result<u32, IndexError> {
        let slice = self.take(4)?;
        let mut value = [0_u8; 4];
        value.copy_from_slice(slice);
        Ok(u32::from_be_bytes(value))
    }

    pub(crate) fn u64(&mut self) -> Result<u64, IndexError> {
        let slice = self.take(8)?;
        let mut value = [0_u8; 8];
        value.copy_from_slice(slice);
        Ok(u64::from_be_bytes(value))
    }

    pub(crate) fn bytes(&mut self) -> Result<&'a [u8], IndexError> {
        let length = self.u64()?;
        let length = usize::try_from(length).map_err(|_| IndexError::IndexMismatch)?;
        self.take(length)
    }

    pub(crate) fn text(&mut self) -> Result<String, IndexError> {
        let bytes = self.bytes()?;
        String::from_utf8(bytes.to_vec()).map_err(|_| IndexError::IndexMismatch)
    }
}
