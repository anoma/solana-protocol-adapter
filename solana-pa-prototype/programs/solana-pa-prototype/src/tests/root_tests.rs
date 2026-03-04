//! Unit tests for root module.

use crate::merkle::PADDING_LEAF;
use crate::root::{create_root_marker, derive_root_pda, is_root_valid, ROOT_SEED};
use crate::state::PAStateAccount;
use crate::tests::utils::create_mock_pa_state;
use crate::types::Digest;
use anchor_lang::prelude::{AccountInfo, Pubkey};

/// Create a PAStateAccount with a specific root for root-validation tests.
fn state_with_root(root: [u8; 32]) -> PAStateAccount {
    let mut state = create_mock_pa_state(Pubkey::new_unique(), false);
    state.root = root;
    state
}

// =========================================================================
// ROOT VALIDATION TESTS
// =========================================================================

#[test]
fn test_is_root_valid_current_root() {
    let state = state_with_root([0xAA; 32]);
    let program_id = Pubkey::new_unique();
    let pa_state_key = Pubkey::new_unique();
    let root = Digest::from_bytes([0xAA; 32]);

    assert!(is_root_valid(
        &state,
        &program_id,
        &pa_state_key,
        &root,
        &[]
    ));
}

#[test]
fn test_is_root_valid_padding_leaf() {
    let state = state_with_root([0xBB; 32]);
    let program_id = Pubkey::new_unique();
    let pa_state_key = Pubkey::new_unique();

    assert!(is_root_valid(
        &state,
        &program_id,
        &pa_state_key,
        &PADDING_LEAF,
        &[]
    ));
}

#[test]
fn test_is_root_valid_non_current_without_marker() {
    let state = state_with_root([0xAA; 32]);
    let program_id = Pubkey::new_unique();
    let pa_state_key = Pubkey::new_unique();
    let old_root = Digest::from_bytes([0xBB; 32]);

    assert!(!is_root_valid(
        &state,
        &program_id,
        &pa_state_key,
        &old_root,
        &[]
    ));
}

// =========================================================================
// PDA DERIVATION TESTS
// =========================================================================

#[test]
fn test_derive_root_pda_deterministic() {
    let program_id = Pubkey::new_unique();
    let pa_state = Pubkey::new_unique();
    let root_bytes = [0xAA; 32];

    let (pda1, bump1) = derive_root_pda(&program_id, &pa_state, &root_bytes);
    let (pda2, bump2) = derive_root_pda(&program_id, &pa_state, &root_bytes);

    assert_eq!(pda1, pda2, "PDA derivation should be deterministic");
    assert_eq!(bump1, bump2, "Bump should be deterministic");
}

#[test]
fn test_derive_root_pda_different_roots_different_pdas() {
    let program_id = Pubkey::new_unique();
    let pa_state = Pubkey::new_unique();
    let root1 = [0xAA; 32];
    let root2 = [0xBB; 32];

    let (pda1, _) = derive_root_pda(&program_id, &pa_state, &root1);
    let (pda2, _) = derive_root_pda(&program_id, &pa_state, &root2);

    assert_ne!(pda1, pda2, "Different roots should have different PDAs");
}

#[test]
fn test_derive_root_pda_different_pa_states_different_pdas() {
    let program_id = Pubkey::new_unique();
    let pa_state1 = Pubkey::new_unique();
    let pa_state2 = Pubkey::new_unique();
    let root = [0xAA; 32];

    let (pda1, _) = derive_root_pda(&program_id, &pa_state1, &root);
    let (pda2, _) = derive_root_pda(&program_id, &pa_state2, &root);

    assert_ne!(
        pda1, pda2,
        "Same root under different PA states should have different PDAs"
    );
}

#[test]
fn test_derive_root_pda_different_programs_different_pdas() {
    let program_id1 = Pubkey::new_unique();
    let program_id2 = Pubkey::new_unique();
    let pa_state = Pubkey::new_unique();
    let root = [0xAA; 32];

    let (pda1, _) = derive_root_pda(&program_id1, &pa_state, &root);
    let (pda2, _) = derive_root_pda(&program_id2, &pa_state, &root);

    assert_ne!(
        pda1, pda2,
        "Same root under different programs should have different PDAs"
    );
}

#[test]
fn test_derive_root_pda_off_curve() {
    let program_id = Pubkey::new_unique();
    let pa_state = Pubkey::new_unique();
    let root = [0x11; 32];

    let (pda, bump) = derive_root_pda(&program_id, &pa_state, &root);

    let recreated = Pubkey::create_program_address(
        &[ROOT_SEED, pa_state.as_ref(), &root, &[bump]],
        &program_id,
    );
    assert!(recreated.is_ok(), "PDA should be recreatable with bump");
    assert_eq!(pda, recreated.unwrap(), "Recreated PDA should match");
}

// =========================================================================
// HISTORICAL ROOT MARKER PDA TESTS
// =========================================================================

#[test]
fn test_is_root_valid_with_historical_marker() {
    let program_id = Pubkey::new_unique();
    let pa_state_key = Pubkey::new_unique();
    let state = state_with_root([0xAA; 32]);
    let old_root = [0xBB; 32];

    let (expected_pda, _) = derive_root_pda(&program_id, &pa_state_key, &old_root);

    let mut lamports = 1u64;
    let mut data = vec![];
    let marker = AccountInfo::new(
        &expected_pda,
        false,
        false,
        &mut lamports,
        &mut data,
        &program_id,
        false,
        0,
    );

    assert!(
        is_root_valid(
            &state,
            &program_id,
            &pa_state_key,
            &Digest::from_bytes(old_root),
            &[marker],
        ),
        "Historical root with valid marker PDA should be accepted"
    );
}

#[test]
fn test_is_root_valid_marker_wrong_owner() {
    let program_id = Pubkey::new_unique();
    let wrong_owner = Pubkey::new_unique();
    let pa_state_key = Pubkey::new_unique();
    let state = state_with_root([0xAA; 32]);
    let old_root = [0xBB; 32];

    let (expected_pda, _) = derive_root_pda(&program_id, &pa_state_key, &old_root);

    let mut lamports = 1u64;
    let mut data = vec![];
    let marker = AccountInfo::new(
        &expected_pda,
        false,
        false,
        &mut lamports,
        &mut data,
        &wrong_owner,
        false,
        0,
    );

    assert!(
        !is_root_valid(
            &state,
            &program_id,
            &pa_state_key,
            &Digest::from_bytes(old_root),
            &[marker],
        ),
        "Marker with wrong owner should be rejected"
    );
}

// =========================================================================
// CREATE ROOT MARKER GUARD PATH TESTS
// =========================================================================

#[test]
fn test_create_root_marker_pda_mismatch() {
    let program_id = Pubkey::new_unique();
    let pa_state_key = Pubkey::new_unique();
    let root_bytes = [0xCC; 32];
    let system_program_id = anchor_lang::solana_program::system_program::ID;

    // Use a wrong key that doesn't match the derived PDA.
    let wrong_key = Pubkey::new_unique();

    let mut payer_lamports = 1_000_000u64;
    let mut payer_data = vec![];
    let payer = AccountInfo::new(
        &pa_state_key,
        true,
        false,
        &mut payer_lamports,
        &mut payer_data,
        &system_program_id,
        false,
        0,
    );

    let mut marker_lamports = 0u64;
    let mut marker_data = vec![];
    let marker = AccountInfo::new(
        &wrong_key,
        false,
        true,
        &mut marker_lamports,
        &mut marker_data,
        &system_program_id,
        false,
        0,
    );

    let mut sys_lamports = 0u64;
    let mut sys_data = vec![];
    let system_program = AccountInfo::new(
        &system_program_id,
        false,
        false,
        &mut sys_lamports,
        &mut sys_data,
        &system_program_id,
        true,
        0,
    );

    let result = create_root_marker(
        &program_id,
        &pa_state_key,
        &root_bytes,
        &payer,
        &marker,
        &system_program,
    );
    assert!(
        result.is_err(),
        "Should fail with RootPdaMismatch when marker key doesn't match derived PDA"
    );
}

#[test]
fn test_create_root_marker_idempotent_existing() {
    let program_id = Pubkey::new_unique();
    let pa_state_key = Pubkey::new_unique();
    let root_bytes = [0xDD; 32];
    let system_program_id = anchor_lang::solana_program::system_program::ID;

    let (expected_pda, _) = derive_root_pda(&program_id, &pa_state_key, &root_bytes);

    let mut payer_lamports = 1_000_000u64;
    let mut payer_data = vec![];
    let payer = AccountInfo::new(
        &pa_state_key,
        true,
        false,
        &mut payer_lamports,
        &mut payer_data,
        &system_program_id,
        false,
        0,
    );

    let mut marker_lamports = 1u64;
    let mut marker_data = vec![];
    let marker = AccountInfo::new(
        &expected_pda,
        false,
        true,
        &mut marker_lamports,
        &mut marker_data,
        &program_id, // Already owned by program → idempotent success
        false,
        0,
    );

    let mut sys_lamports = 0u64;
    let mut sys_data = vec![];
    let system_program = AccountInfo::new(
        &system_program_id,
        false,
        false,
        &mut sys_lamports,
        &mut sys_data,
        &system_program_id,
        true,
        0,
    );

    let result = create_root_marker(
        &program_id,
        &pa_state_key,
        &root_bytes,
        &payer,
        &marker,
        &system_program,
    );
    assert!(
        result.is_ok(),
        "Should return Ok when marker already exists (idempotent)"
    );
}
