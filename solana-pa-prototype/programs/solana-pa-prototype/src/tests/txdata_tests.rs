//! Unit tests for txdata module.

use crate::error::PAError;
use crate::txdata::TxData;

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
fn test_txdata_expiry_boundary_exact() {
    let txdata = TxData::new(100, 500);
    // At exactly expiry_slot, not yet expired (check is current > expiry)
    assert!(!txdata.is_expired(500));
    // One slot after, expired
    assert!(txdata.is_expired(501));
}

#[test]
fn test_txdata_validate_not_expired_passes() {
    let txdata = TxData::new(100, 1000);
    // Should pass when not expired
    assert!(txdata.validate_not_expired(999).is_ok());
    // Should pass at exact boundary
    assert!(txdata.validate_not_expired(1000).is_ok());
}

#[test]
fn test_txdata_validate_not_expired_fails() {
    let txdata = TxData::new(100, 1000);
    let result = txdata.validate_not_expired(1001);
    assert!(result.is_err());
    match result {
        Err(PAError::TxDataExpired) => {}
        _ => panic!("Expected TxDataExpired error"),
    }
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
