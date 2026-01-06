//! Unit tests for nullifier module.

use anchor_lang::prelude::Pubkey;
use crate::nullifier::{derive_nullifier_pda, NULLIFIER_SEED};

// -------------------------------------------------------------------------
// derive_nullifier_pda tests
// -------------------------------------------------------------------------

#[test]
fn test_derive_nullifier_pda_deterministic() {
    // Same inputs should always produce the same PDA
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
    let nullifier1 = [0xAA; 32];
    let nullifier2 = [0xBB; 32];

    let (pda1, _) = derive_nullifier_pda(&program_id, &pa_state, &nullifier1);
    let (pda2, _) = derive_nullifier_pda(&program_id, &pa_state, &nullifier2);

    assert_ne!(pda1, pda2, "Different nullifiers should have different PDAs");
}

#[test]
fn test_derive_nullifier_pda_different_pa_states_different_pdas() {
    // Nullifiers are scoped to PA state, so same nullifier under different PA states
    // should produce different PDAs
    let program_id = Pubkey::new_unique();
    let pa_state1 = Pubkey::new_unique();
    let pa_state2 = Pubkey::new_unique();
    let nullifier = [0xAA; 32];

    let (pda1, _) = derive_nullifier_pda(&program_id, &pa_state1, &nullifier);
    let (pda2, _) = derive_nullifier_pda(&program_id, &pa_state2, &nullifier);

    assert_ne!(pda1, pda2, "Same nullifier under different PA states should have different PDAs");
}

#[test]
fn test_derive_nullifier_pda_different_programs_different_pdas() {
    let program_id1 = Pubkey::new_unique();
    let program_id2 = Pubkey::new_unique();
    let pa_state = Pubkey::new_unique();
    let nullifier = [0xAA; 32];

    let (pda1, _) = derive_nullifier_pda(&program_id1, &pa_state, &nullifier);
    let (pda2, _) = derive_nullifier_pda(&program_id2, &pa_state, &nullifier);

    assert_ne!(pda1, pda2, "Same nullifier under different programs should have different PDAs");
}

#[test]
fn test_derive_nullifier_pda_off_curve() {
    // PDAs must be off the ed25519 curve
    let program_id = Pubkey::new_unique();
    let pa_state = Pubkey::new_unique();
    let nullifier = [0x11; 32];

    let (pda, bump) = derive_nullifier_pda(&program_id, &pa_state, &nullifier);

    // Verify we can recreate the PDA with the bump
    let recreated = Pubkey::create_program_address(
        &[NULLIFIER_SEED, pa_state.as_ref(), &nullifier, &[bump]],
        &program_id,
    );
    assert!(recreated.is_ok(), "PDA should be recreatable with bump");
    assert_eq!(pda, recreated.unwrap(), "Recreated PDA should match");
}

#[test]
fn test_derive_nullifier_pda_all_zeros_nullifier() {
    // Edge case: all-zeros nullifier should work
    let program_id = Pubkey::new_unique();
    let pa_state = Pubkey::new_unique();
    let nullifier = [0x00; 32];

    let (pda, bump) = derive_nullifier_pda(&program_id, &pa_state, &nullifier);

    // Should produce valid PDA
    let recreated = Pubkey::create_program_address(
        &[NULLIFIER_SEED, pa_state.as_ref(), &nullifier, &[bump]],
        &program_id,
    );
    assert!(recreated.is_ok(), "All-zeros nullifier should produce valid PDA");
    assert_eq!(pda, recreated.unwrap());
}

#[test]
fn test_derive_nullifier_pda_all_ones_nullifier() {
    // Edge case: all-ones nullifier should work
    let program_id = Pubkey::new_unique();
    let pa_state = Pubkey::new_unique();
    let nullifier = [0xFF; 32];

    let (pda, bump) = derive_nullifier_pda(&program_id, &pa_state, &nullifier);

    // Should produce valid PDA
    let recreated = Pubkey::create_program_address(
        &[NULLIFIER_SEED, pa_state.as_ref(), &nullifier, &[bump]],
        &program_id,
    );
    assert!(recreated.is_ok(), "All-ones nullifier should produce valid PDA");
    assert_eq!(pda, recreated.unwrap());
}

// -------------------------------------------------------------------------
// Seed constant tests
// -------------------------------------------------------------------------

#[test]
fn test_nullifier_seed_is_correct() {
    assert_eq!(NULLIFIER_SEED, b"nullifier");
}
