//! TxData upload mechanism for large transactions.

use crate::error::PAError;

/// Account for uploading large transaction data in chunks.
pub struct TxData {
    capacity: usize,
    expiry_slot: u64,
    data: Vec<u8>,
    written_len: usize,
}

impl TxData {
    pub fn new(capacity: usize, expiry_slot: u64) -> Self {
        Self {
            capacity,
            expiry_slot,
            data: vec![0u8; capacity],
            written_len: 0,
        }
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    pub fn written_len(&self) -> usize {
        self.written_len
    }

    /// Write data at the given offset. Last-write-wins semantics.
    pub fn write(&mut self, offset: usize, data: &[u8]) -> Result<(), PAError> {
        let end = offset
            .checked_add(data.len())
            .ok_or(PAError::TxDataBoundsExceeded)?;
        if end > self.capacity {
            return Err(PAError::TxDataBoundsExceeded);
        }

        self.data[offset..end].copy_from_slice(data);
        self.written_len = self.written_len.max(end);

        Ok(())
    }

    pub fn payload(&self) -> &[u8] {
        &self.data[..self.written_len]
    }

    pub fn is_expired(&self, current_slot: u64) -> bool {
        current_slot > self.expiry_slot
    }

    pub fn validate_not_expired(&self, current_slot: u64) -> Result<(), PAError> {
        if self.is_expired(current_slot) {
            return Err(PAError::TxDataExpired);
        }
        Ok(())
    }
}
