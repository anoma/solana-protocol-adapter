use crate::root::{create_root_marker, derive_root_pda, is_root_valid};
use crate::state::PAStateAccount;
use crate::tests::utils::{
    assert_anchor_err, create_test_pa_state, make_account_info, make_account_info_with_data,
};
use anchor_lang::prelude::Pubkey;
use arm_core::merkle_path::PADDING_LEAF;
use arm_core::Digest;

fn state_with_root(root: [u8; 32]) -> PAStateAccount {
    let mut state = create_test_pa_state();
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
    assert_anchor_err!(result, RootPdaMismatch);
}

/// A repeated produced root means the tree stopped advancing. Fail loudly rather
/// than silently reusing the marker.
#[test]
fn test_create_root_marker_rejects_existing_marker() {
    let program_id = Pubkey::new_unique();
    let pa_state_key = Pubkey::new_unique();
    let root_bytes = [0xDD; 32];
    let system_program_id = anchor_lang::solana_program::system_program::ID;

    let (expected_pda, _) = derive_root_pda(&program_id, &pa_state_key, &root_bytes);

    make_account_info!(payer, &pa_state_key, owner: &system_program_id,
        lamports: 1_000_000, signer: true, writable: false, executable: false);
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
    assert_anchor_err!(result, RootMarkerAlreadyExists);
}

#[test]
fn test_is_root_valid_marker_wrong_key() {
    let program_id = Pubkey::new_unique();
    let pa_state_key = Pubkey::new_unique();
    let state = state_with_root([0xAA; 32]);
    let old_root = [0xBB; 32];
    let wrong_key = Pubkey::new_unique();

    make_account_info!(marker, &wrong_key, owner: &program_id,
        lamports: 1, signer: false, writable: false, executable: false);

    assert!(
        !is_root_valid(
            &state,
            &program_id,
            &pa_state_key,
            &Digest::from_bytes(old_root),
            &[marker],
        ),
        "Marker with correct owner but wrong key should be rejected"
    );
}

/// A foreign-owned account at the marker address must fail closed, never be
/// taken over.
#[test]
fn test_create_root_marker_rejects_foreign_owner() {
    let program_id = Pubkey::new_unique();
    let pa_state_key = Pubkey::new_unique();
    let root_bytes = [0xEE; 32];
    let system_program_id = anchor_lang::solana_program::system_program::ID;
    let foreign_owner = Pubkey::new_unique();

    let (expected_pda, _) = derive_root_pda(&program_id, &pa_state_key, &root_bytes);

    make_account_info!(payer, &pa_state_key, owner: &system_program_id,
        lamports: 1_000_000, signer: true, writable: false, executable: false);
    make_account_info!(marker, &expected_pda, owner: &foreign_owner,
        lamports: 890_880, signer: false, writable: true, executable: false);
    make_account_info!(system_program, &system_program_id, owner: &system_program_id,
        lamports: 0, signer: false, writable: false, executable: true);

    let result = create_root_marker(
        &program_id,
        &pa_state_key,
        &root_bytes,
        &payer,
        &marker,
        &system_program,
        890_880,
    );
    assert_anchor_err!(result, MarkerUnexpectedOwner);
}

/// An account carrying data must fail closed.
#[test]
fn test_create_root_marker_rejects_account_with_data() {
    let program_id = Pubkey::new_unique();
    let pa_state_key = Pubkey::new_unique();
    let root_bytes = [0xEF; 32];
    let system_program_id = anchor_lang::solana_program::system_program::ID;

    let (expected_pda, _) = derive_root_pda(&program_id, &pa_state_key, &root_bytes);

    make_account_info!(payer, &pa_state_key, owner: &system_program_id,
        lamports: 1_000_000, signer: true, writable: false, executable: false);
    make_account_info_with_data!(marker, &expected_pda, owner: &system_program_id,
        lamports: 890_880, data: vec![0u8; 8], signer: false, writable: true,
        executable: false);
    make_account_info!(system_program, &system_program_id, owner: &system_program_id,
        lamports: 0, signer: false, writable: false, executable: true);

    let result = create_root_marker(
        &program_id,
        &pa_state_key,
        &root_bytes,
        &payer,
        &marker,
        &system_program,
        890_880,
    );
    assert_anchor_err!(result, MarkerUnexpectedData);
}

/// The empty-tree root is accepted permanently through the PADDING_LEAF branch,
/// with no marker account, even after the tree has advanced. This is why
/// initialization does not create a genesis marker.
#[test]
fn test_empty_tree_root_valid_without_marker_after_tree_advances() {
    use crate::merkle::EMPTY_TREE_ROOT_INITIAL;

    // The tree has moved on: current root is something else entirely.
    let state = state_with_root([0x99; 32]);
    let program_id = Pubkey::new_unique();
    let pa_state_key = Pubkey::new_unique();

    assert_eq!(
        EMPTY_TREE_ROOT_INITIAL, PADDING_LEAF,
        "genesis root must equal PADDING_LEAF for the branch to cover it"
    );
    assert!(
        is_root_valid(
            &state,
            &program_id,
            &pa_state_key,
            &EMPTY_TREE_ROOT_INITIAL,
            &[]
        ),
        "empty-tree root must remain valid with no marker supplied"
    );
}
