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
        let end = offset.checked_add(data.len()).ok_or(PAError::TxDataBoundsExceeded)?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_txdata_init() {
        let txdata = TxData::new(1000, 100);
        assert_eq!(txdata.capacity(), 1000);
        assert_eq!(txdata.written_len(), 0);
    }

    #[test]
    fn test_txdata_write_sequential() {
        let mut txdata = TxData::new(100, 1000);

        txdata.write(0, &[1, 2, 3, 4]).unwrap();
        assert_eq!(txdata.written_len(), 4);

        txdata.write(4, &[5, 6, 7, 8]).unwrap();
        assert_eq!(txdata.written_len(), 8);
    }

    #[test]
    fn test_txdata_write_overwrites() {
        let mut txdata = TxData::new(100, 1000);

        txdata.write(0, &[1, 2, 3, 4]).unwrap();
        // Overwrite with different data (last-write-wins)
        txdata.write(0, &[5, 6, 7, 8]).unwrap();
        assert_eq!(txdata.written_len(), 4);
        assert_eq!(txdata.payload(), &[5, 6, 7, 8]);
    }

    #[test]
    fn test_txdata_write_with_gap() {
        let mut txdata = TxData::new(100, 1000);
        txdata.write(0, &[1, 2, 3, 4]).unwrap();

        // Write with a gap is now allowed (last-write-wins)
        txdata.write(10, &[5, 6, 7, 8]).unwrap();
        assert_eq!(txdata.written_len(), 14);
    }

    #[test]
    fn test_txdata_read_payload() {
        let mut txdata = TxData::new(8, 1000);
        let data = [1, 2, 3, 4, 5, 6, 7, 8];
        txdata.write(0, &data).unwrap();

        let payload = txdata.payload();
        assert_eq!(payload, &data);
    }

    #[test]
    fn test_txdata_expiry() {
        let txdata = TxData::new(100, 1000);
        assert!(!txdata.is_expired(999));
        assert!(txdata.is_expired(1001));
    }

    #[test]
    fn test_txdata_bounds_exceeded() {
        let mut txdata = TxData::new(4, 1000);

        // Writing beyond capacity should fail
        let result = txdata.write(0, &[1, 2, 3, 4, 5, 6, 7, 8]);
        assert!(result.is_err());
    }

    #[test]
    fn test_txdata_empty_payload() {
        let txdata = TxData::new(0, 1000);
        assert_eq!(txdata.capacity(), 0);
        assert_eq!(txdata.written_len(), 0);
        assert!(txdata.payload().is_empty());
    }
}
