//! Unit tests for root module.

use crate::merkle::{INITIAL_TREE_DEPTH, PADDING_LEAF};
use crate::root::{derive_root_marker_pda, is_root_valid, ROOT_MARKER_SEED};
use crate::state::{PAStateAccount, MAX_EXPIRY_SLOTS, MIN_EXPIRY_SLOTS};
use crate::types::Digest;
use anchor_lang::prelude::Pubkey;

// =========================================================================
// ROOT VALIDATION TESTS
// =========================================================================

#[test]
fn test_is_root_valid_current_root() {
    let state = PAStateAccount {
        bump: 0,
        authority: Pubkey::new_unique(),
        paused: false,
        root: [0xAA; 32],
        next_index: 0,
        current_depth: INITIAL_TREE_DEPTH as u8,
        frontier: vec![[0; 32]],
        min_expiry_slots: MIN_EXPIRY_SLOTS,
        max_expiry_slots: MAX_EXPIRY_SLOTS,
    };
    let pa_state_key = Pubkey::new_unique();
    let root = Digest::from_bytes([0xAA; 32]);

    assert!(is_root_valid(&state, &pa_state_key, &root, &[]));
}

#[test]
fn test_is_root_valid_padding_leaf() {
    let state = PAStateAccount {
        bump: 0,
        authority: Pubkey::new_unique(),
        paused: false,
        root: [0xBB; 32],
        next_index: 0,
        current_depth: INITIAL_TREE_DEPTH as u8,
        frontier: vec![[0; 32]],
        min_expiry_slots: MIN_EXPIRY_SLOTS,
        max_expiry_slots: MAX_EXPIRY_SLOTS,
    };
    let pa_state_key = Pubkey::new_unique();

    assert!(is_root_valid(&state, &pa_state_key, &PADDING_LEAF, &[]));
}

#[test]
fn test_is_root_valid_non_current_without_marker() {
    let state = PAStateAccount {
        bump: 0,
        authority: Pubkey::new_unique(),
        paused: false,
        root: [0xAA; 32],
        next_index: 0,
        current_depth: INITIAL_TREE_DEPTH as u8,
        frontier: vec![[0; 32]],
        min_expiry_slots: MIN_EXPIRY_SLOTS,
        max_expiry_slots: MAX_EXPIRY_SLOTS,
    };
    let pa_state_key = Pubkey::new_unique();
    let old_root = Digest::from_bytes([0xBB; 32]);

    assert!(!is_root_valid(&state, &pa_state_key, &old_root, &[]));
}

// =========================================================================
// PDA DERIVATION TESTS
// =========================================================================

#[test]
fn test_derive_root_marker_pda_deterministic() {
    let program_id = Pubkey::new_unique();
    let pa_state = Pubkey::new_unique();
    let root_bytes = [0xAA; 32];

    let (pda1, bump1) = derive_root_marker_pda(&program_id, &pa_state, &root_bytes);
    let (pda2, bump2) = derive_root_marker_pda(&program_id, &pa_state, &root_bytes);

    assert_eq!(pda1, pda2, "PDA derivation should be deterministic");
    assert_eq!(bump1, bump2, "Bump should be deterministic");
}

#[test]
fn test_derive_root_marker_pda_different_roots_different_pdas() {
    let program_id = Pubkey::new_unique();
    let pa_state = Pubkey::new_unique();
    let root1 = [0xAA; 32];
    let root2 = [0xBB; 32];

    let (pda1, _) = derive_root_marker_pda(&program_id, &pa_state, &root1);
    let (pda2, _) = derive_root_marker_pda(&program_id, &pa_state, &root2);

    assert_ne!(pda1, pda2, "Different roots should have different PDAs");
}

#[test]
fn test_derive_root_marker_pda_different_pa_states_different_pdas() {
    let program_id = Pubkey::new_unique();
    let pa_state1 = Pubkey::new_unique();
    let pa_state2 = Pubkey::new_unique();
    let root = [0xAA; 32];

    let (pda1, _) = derive_root_marker_pda(&program_id, &pa_state1, &root);
    let (pda2, _) = derive_root_marker_pda(&program_id, &pa_state2, &root);

    assert_ne!(
        pda1, pda2,
        "Same root under different PA states should have different PDAs"
    );
}

#[test]
fn test_derive_root_marker_pda_off_curve() {
    let program_id = Pubkey::new_unique();
    let pa_state = Pubkey::new_unique();
    let root = [0x11; 32];

    let (pda, bump) = derive_root_marker_pda(&program_id, &pa_state, &root);

    let recreated = Pubkey::create_program_address(
        &[ROOT_MARKER_SEED, pa_state.as_ref(), &root, &[bump]],
        &program_id,
    );
    assert!(recreated.is_ok(), "PDA should be recreatable with bump");
    assert_eq!(pda, recreated.unwrap(), "Recreated PDA should match");
}

#[test]
fn test_root_marker_seed_is_correct() {
    assert_eq!(ROOT_MARKER_SEED, b"root");
}
