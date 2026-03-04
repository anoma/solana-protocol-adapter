use crate::nullifier::{check_and_create_nullifier_marker, derive_nullifier_pda, NULLIFIER_SEED};
use crate::tests::utils::make_account_info;
use anchor_lang::prelude::Pubkey;

fn assert_pda_recreatable(nullifier: &[u8; 32]) {
    let program_id = Pubkey::new_unique();
    let pa_state = Pubkey::new_unique();

    let (pda, bump) = derive_nullifier_pda(&program_id, &pa_state, nullifier);

    let recreated = Pubkey::create_program_address(
        &[NULLIFIER_SEED, pa_state.as_ref(), nullifier, &[bump]],
        &program_id,
    )
    .expect("PDA should be recreatable with bump");

    assert_eq!(
        pda,
        recreated,
        "Recreated PDA should match for nullifier {:02x?}",
        &nullifier[..4]
    );
}

#[test]
fn test_derive_nullifier_pda_deterministic() {
    let program_id = Pubkey::new_unique();
    let pa_state = Pubkey::new_unique();
    let nullifier_bytes = [0xAA; 32];

    let (pda1, bump1) = derive_nullifier_pda(&program_id, &pa_state, &nullifier_bytes);
    let (pda2, bump2) = derive_nullifier_pda(&program_id, &pa_state, &nullifier_bytes);

    assert_eq!(pda1, pda2, "PDA derivation should be deterministic");
    assert_eq!(bump1, bump2, "Bump should be deterministic");
}

#[test]
fn test_derive_nullifier_pda_different_nullifiers_different_pdas() {
    let program_id = Pubkey::new_unique();
    let pa_state = Pubkey::new_unique();

    let (pda1, _) = derive_nullifier_pda(&program_id, &pa_state, &[0xAA; 32]);
    let (pda2, _) = derive_nullifier_pda(&program_id, &pa_state, &[0xBB; 32]);

    assert_ne!(
        pda1, pda2,
        "Different nullifiers should have different PDAs"
    );
}

#[test]
fn test_derive_nullifier_pda_different_pa_states_different_pdas() {
    let program_id = Pubkey::new_unique();
    let pa_state1 = Pubkey::new_unique();
    let pa_state2 = Pubkey::new_unique();
    let nullifier = [0xAA; 32];

    let (pda1, _) = derive_nullifier_pda(&program_id, &pa_state1, &nullifier);
    let (pda2, _) = derive_nullifier_pda(&program_id, &pa_state2, &nullifier);

    assert_ne!(
        pda1, pda2,
        "Same nullifier under different PA states should have different PDAs"
    );
}

#[test]
fn test_derive_nullifier_pda_different_programs_different_pdas() {
    let program_id1 = Pubkey::new_unique();
    let program_id2 = Pubkey::new_unique();
    let pa_state = Pubkey::new_unique();
    let nullifier = [0xAA; 32];

    let (pda1, _) = derive_nullifier_pda(&program_id1, &pa_state, &nullifier);
    let (pda2, _) = derive_nullifier_pda(&program_id2, &pa_state, &nullifier);

    assert_ne!(
        pda1, pda2,
        "Same nullifier under different programs should have different PDAs"
    );
}

#[test]
fn test_derive_nullifier_pda_recreatable_with_bump() {
    assert_pda_recreatable(&[0x11; 32]);
}

#[test]
fn test_derive_nullifier_pda_all_zeros() {
    assert_pda_recreatable(&[0x00; 32]);
}

#[test]
fn test_derive_nullifier_pda_all_ones() {
    assert_pda_recreatable(&[0xFF; 32]);
}

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
    assert!(
        result.is_err(),
        "Should fail with NullifierPdaMismatch when marker key doesn't match derived PDA"
    );
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
    assert!(
        result.is_err(),
        "Should fail with DuplicateNullifier when marker is already owned by program"
    );
}
