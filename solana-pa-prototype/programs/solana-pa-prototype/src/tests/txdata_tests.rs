//! Tests TxDataAccount write semantics in isolation (without Anchor context).

use anchor_lang::prelude::Pubkey;

use crate::error::PAError;
use crate::state::TxDataAccount;

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

#[test]
fn test_txdata_write_sequential() {
    let mut txdata = make_txdata(100, 1000);

    txdata.write_chunk(0, &[1, 2, 3, 4]).unwrap();
    assert_eq!(txdata.written_len, 4);

    txdata.write_chunk(4, &[5, 6, 7, 8]).unwrap();
    assert_eq!(txdata.written_len, 8);
}

#[test]
fn test_txdata_write_overwrites() {
    let mut txdata = make_txdata(100, 1000);

    txdata.write_chunk(0, &[1, 2, 3, 4]).unwrap();
    txdata.write_chunk(0, &[5, 6, 7, 8]).unwrap();
    assert_eq!(txdata.written_len, 4);
    assert_eq!(&txdata.payload[..4], &[5, 6, 7, 8]);
}

#[test]
fn test_txdata_write_with_gap() {
    let mut txdata = make_txdata(100, 1000);
    txdata.write_chunk(0, &[1, 2, 3, 4]).unwrap();
    txdata.write_chunk(10, &[5, 6, 7, 8]).unwrap();
    assert_eq!(txdata.written_len, 14);
}

#[test]
fn test_txdata_read_payload() {
    let mut txdata = make_txdata(8, 1000);
    let data = [1, 2, 3, 4, 5, 6, 7, 8];
    txdata.write_chunk(0, &data).unwrap();
    assert_eq!(&txdata.payload[..txdata.written_len as usize], &data);
}

#[test]
fn test_txdata_bounds_exceeded() {
    let mut txdata = make_txdata(4, 1000);
    let result = txdata.write_chunk(0, &[1, 2, 3, 4, 5, 6, 7, 8]);
    assert!(matches!(result, Err(PAError::TxDataBoundsExceeded)));
}
