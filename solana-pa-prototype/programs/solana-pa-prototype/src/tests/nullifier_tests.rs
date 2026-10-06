use crate::nullifier::{check_and_create_nullifier_marker, derive_nullifier_pda};
use crate::tests::utils::{assert_anchor_err, make_account_info, make_account_info_with_data};
use anchor_lang::prelude::Pubkey;

#[test]
fn test_nullifier_pda_mismatch() {
    let program_id = Pubkey::new_unique();
    let pa_state_key = Pubkey::new_unique();
    let nullifier_bytes = [0xAA; 32];
    let system_program_id = anchor_lang::solana_program::system_program::ID;
    let wrong_key = Pubkey::new_unique();

    make_account_info!(payer, &pa_state_key, owner: &system_program_id,
        lamports: 1_000_000, signer: true, writable: false, executable: false);
    make_account_info!(marker, &wrong_key, owner: &system_program_id,
        lamports: 0, signer: false, writable: true, executable: false);
    make_account_info!(system_program, &system_program_id, owner: &system_program_id,
        lamports: 0, signer: false, writable: false, executable: true);

    let result = check_and_create_nullifier_marker(
        &program_id,
        &pa_state_key,
        &nullifier_bytes,
        &payer,
        &marker,
        &system_program,
        1,
    );
    assert_anchor_err!(result, NullifierPdaMismatch);
}

#[test]
fn test_duplicate_nullifier_detected() {
    let program_id = Pubkey::new_unique();
    let pa_state_key = Pubkey::new_unique();
    let nullifier_bytes = [0xBB; 32];
    let system_program_id = anchor_lang::solana_program::system_program::ID;
    let (expected_pda, _) = derive_nullifier_pda(&program_id, &pa_state_key, &nullifier_bytes);

    make_account_info!(payer, &pa_state_key, owner: &system_program_id,
        lamports: 1_000_000, signer: true, writable: false, executable: false);
    // Marker already owned by our program signals a duplicate.
    make_account_info!(marker, &expected_pda, owner: &program_id,
        lamports: 1, signer: false, writable: true, executable: false);
    make_account_info!(system_program, &system_program_id, owner: &system_program_id,
        lamports: 0, signer: false, writable: false, executable: true);

    let result = check_and_create_nullifier_marker(
        &program_id,
        &pa_state_key,
        &nullifier_bytes,
        &payer,
        &marker,
        &system_program,
        1,
    );
    assert_anchor_err!(result, PreExistingNullifier);
}

/// A foreign-owned account at the marker address must fail closed, never be
/// taken over.
#[test]
fn test_create_nullifier_marker_rejects_foreign_owner() {
    let program_id = Pubkey::new_unique();
    let pa_state_key = Pubkey::new_unique();
    let nullifier_bytes = [0xEE; 32];
    let system_program_id = anchor_lang::solana_program::system_program::ID;
    let foreign_owner = Pubkey::new_unique();

    let (expected_pda, _) = derive_nullifier_pda(&program_id, &pa_state_key, &nullifier_bytes);

    make_account_info!(payer, &pa_state_key, owner: &system_program_id,
        lamports: 1_000_000, signer: true, writable: false, executable: false);
    make_account_info!(marker, &expected_pda, owner: &foreign_owner,
        lamports: 890_880, signer: false, writable: true, executable: false);
    make_account_info!(system_program, &system_program_id, owner: &system_program_id,
        lamports: 0, signer: false, writable: false, executable: true);

    let result = check_and_create_nullifier_marker(
        &program_id,
        &pa_state_key,
        &nullifier_bytes,
        &payer,
        &marker,
        &system_program,
        890_880,
    );
    assert_anchor_err!(result, MarkerUnexpectedOwner);
}

/// An account carrying data must fail closed.
#[test]
fn test_create_nullifier_marker_rejects_account_with_data() {
    let program_id = Pubkey::new_unique();
    let pa_state_key = Pubkey::new_unique();
    let nullifier_bytes = [0xEF; 32];
    let system_program_id = anchor_lang::solana_program::system_program::ID;

    let (expected_pda, _) = derive_nullifier_pda(&program_id, &pa_state_key, &nullifier_bytes);

    make_account_info!(payer, &pa_state_key, owner: &system_program_id,
        lamports: 1_000_000, signer: true, writable: false, executable: false);
    make_account_info_with_data!(marker, &expected_pda, owner: &system_program_id,
        lamports: 890_880, data: vec![0u8; 8], signer: false, writable: true,
        executable: false);
    make_account_info!(system_program, &system_program_id, owner: &system_program_id,
        lamports: 0, signer: false, writable: false, executable: true);

    let result = check_and_create_nullifier_marker(
        &program_id,
        &pa_state_key,
        &nullifier_bytes,
        &payer,
        &marker,
        &system_program,
        890_880,
    );
    assert_anchor_err!(result, MarkerUnexpectedData);
}
