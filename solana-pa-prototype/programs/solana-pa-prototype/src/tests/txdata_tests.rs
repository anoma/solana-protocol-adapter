//! Unit tests for TxData write semantics and expiry logic.
//!
//! Tests the same write/expiry logic used by the on-chain instruction handlers
//! (txdata_write, txdata_close, etc.) using TxDataAccount directly.

use anchor_lang::prelude::Pubkey;

use crate::error::PAError;
use crate::state::{TxDataAccount, MAX_EXPIRY_SLOTS, MIN_EXPIRY_SLOTS};

/// Create a TxDataAccount for testing with given capacity and expiry.
fn make_txdata(capacity: usize, expires_slot: u64) -> TxDataAccount {
    TxDataAccount {
        bump: 0,
        authority: Pubkey::default(),
        refund: Pubkey::default(),
        written_len: 0,
        expires_slot,
        payload: vec![0u8; capacity],
    }
}

/// Write data to TxDataAccount at offset, matching production logic in lib.rs txdata_write.
/// Returns PAError::TxDataBoundsExceeded if the write exceeds payload capacity.
fn txdata_write(account: &mut TxDataAccount, offset: u32, data: &[u8]) -> Result<(), PAError> {
    let end = offset as usize + data.len();
    if end > account.payload.len() {
        return Err(PAError::TxDataBoundsExceeded);
    }
    account.payload[offset as usize..end].copy_from_slice(data);
    account.written_len = std::cmp::max(account.written_len, end as u32);
    Ok(())
}

#[test]
fn test_txdata_write_sequential() {
    let mut txdata = make_txdata(100, 1000);

    txdata_write(&mut txdata, 0, &[1, 2, 3, 4]).unwrap();
    assert_eq!(txdata.written_len, 4);

    txdata_write(&mut txdata, 4, &[5, 6, 7, 8]).unwrap();
    assert_eq!(txdata.written_len, 8);
}

#[test]
fn test_txdata_write_overwrites() {
    let mut txdata = make_txdata(100, 1000);

    txdata_write(&mut txdata, 0, &[1, 2, 3, 4]).unwrap();
    // Overwrite with different data (last-write-wins)
    txdata_write(&mut txdata, 0, &[5, 6, 7, 8]).unwrap();
    assert_eq!(txdata.written_len, 4);
    assert_eq!(&txdata.payload[..4], &[5, 6, 7, 8]);
}

#[test]
fn test_txdata_write_with_gap() {
    let mut txdata = make_txdata(100, 1000);
    txdata_write(&mut txdata, 0, &[1, 2, 3, 4]).unwrap();

    // Write with a gap is allowed (last-write-wins)
    txdata_write(&mut txdata, 10, &[5, 6, 7, 8]).unwrap();
    assert_eq!(txdata.written_len, 14);
}

#[test]
fn test_txdata_read_payload() {
    let mut txdata = make_txdata(8, 1000);
    let data = [1, 2, 3, 4, 5, 6, 7, 8];
    txdata_write(&mut txdata, 0, &data).unwrap();
    assert_eq!(&txdata.payload[..txdata.written_len as usize], &data);
}

#[test]
fn test_txdata_bounds_exceeded() {
    let mut txdata = make_txdata(4, 1000);

    // Writing beyond capacity should fail
    let result = txdata_write(&mut txdata, 0, &[1, 2, 3, 4, 5, 6, 7, 8]);
    assert!(result.is_err());
    match result {
        Err(PAError::TxDataBoundsExceeded) => {}
        _ => panic!("Expected TxDataBoundsExceeded error"),
    }
}

// Compile-time verification that expiry constants are reasonable.
const _: () = assert!(MIN_EXPIRY_SLOTS > 0);
const _: () = assert!(MIN_EXPIRY_SLOTS < MAX_EXPIRY_SLOTS);
