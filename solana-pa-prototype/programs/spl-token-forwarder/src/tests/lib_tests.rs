//! Tests for lib.rs (constants and program entry points)

use crate::{OP_WRAP, OP_UNWRAP, RESULT_SUCCESS, SPL_TOKEN_PROGRAM_ID};
use anchor_lang::prelude::Pubkey;
use std::str::FromStr;

#[test]
fn test_operation_constants() {
    assert_eq!(OP_WRAP, 0);
    assert_eq!(OP_UNWRAP, 1);
}

#[test]
fn test_result_constants() {
    assert_eq!(RESULT_SUCCESS, 1);
}

#[test]
fn test_spl_token_program_id() {
    let expected = Pubkey::from_str("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA").unwrap();
    assert_eq!(SPL_TOKEN_PROGRAM_ID, expected);
}
