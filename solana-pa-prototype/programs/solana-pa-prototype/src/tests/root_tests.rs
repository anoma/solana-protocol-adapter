use crate::merkle::PADDING_LEAF;
use crate::root::{create_root_marker, derive_root_pda, is_root_valid, ROOT_SEED};
use crate::state::PAStateAccount;
use crate::tests::utils::{create_mock_pa_state, make_account_info};
use crate::types::Digest;
use anchor_lang::prelude::Pubkey;

fn state_with_root(root: [u8; 32]) -> PAStateAccount {
    let mut state = create_mock_pa_state(Pubkey::new_unique(), false);
    state.root = root;
    state
}

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

#[test]
fn test_is_root_valid_with_historical_marker() {
    let program_id = Pubkey::new_unique();
    let pa_state_key = Pubkey::new_unique();
    let state = state_with_root([0xAA; 32]);
    let old_root = [0xBB; 32];

    let (expected_pda, _) = derive_root_pda(&program_id, &pa_state_key, &old_root);

    make_account_info!(marker, &expected_pda, owner: &program_id,
        lamports: 1, signer: false, writable: false, executable: false);

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

    make_account_info!(marker, &expected_pda, owner: &wrong_owner,
        lamports: 1, signer: false, writable: false, executable: false);

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

#[test]
fn test_create_root_marker_pda_mismatch() {
    let program_id = Pubkey::new_unique();
    let pa_state_key = Pubkey::new_unique();
    let root_bytes = [0xCC; 32];
    let system_program_id = anchor_lang::solana_program::system_program::ID;
    let wrong_key = Pubkey::new_unique();

    make_account_info!(payer, &pa_state_key, owner: &system_program_id,
        lamports: 1_000_000, signer: true, writable: false, executable: false);
    make_account_info!(marker, &wrong_key, owner: &system_program_id,
        lamports: 0, signer: false, writable: true, executable: false);
    make_account_info!(system_program, &system_program_id, owner: &system_program_id,
        lamports: 0, signer: false, writable: false, executable: true);

    let result = create_root_marker(
        &program_id,
        &pa_state_key,
        &root_bytes,
        &payer,
        &marker,
        &system_program,
        1,
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

    make_account_info!(payer, &pa_state_key, owner: &system_program_id,
        lamports: 1_000_000, signer: true, writable: false, executable: false);
    // Already owned by program → idempotent success
    make_account_info!(marker, &expected_pda, owner: &program_id,
        lamports: 1, signer: false, writable: true, executable: false);
    make_account_info!(system_program, &system_program_id, owner: &system_program_id,
        lamports: 0, signer: false, writable: false, executable: true);

    let result = create_root_marker(
        &program_id,
        &pa_state_key,
        &root_bytes,
        &payer,
        &marker,
        &system_program,
        1,
    );
    assert!(
        result.is_ok(),
        "Should return Ok when marker already exists (idempotent)"
    );
}
