//! Unit tests for merkle module.

use crate::compute_root_from_frontier;
use crate::merkle::{EMPTY_TREE_ROOT_INITIAL, PADDING_LEAF, ZEROS};
use crate::tests::utils::create_test_pa_state;
use crate::types::Digest;
use anchor_lang::solana_program::hash::hashv;
use sha2::{Digest as Sha2Digest, Sha256};

/// Verify that solana_program::hash and sha2::Sha256 produce identical output.
/// This is critical for switching from sha2 crate to syscall without breaking
/// compatibility with arm-risc0's merkle tree format.
#[test]
fn test_sha256_syscall_matches_sha2_crate() {
    // Test with PADDING_LEAF bytes (the base case for merkle tree)
    let left = PADDING_LEAF.to_bytes();
    let right = PADDING_LEAF.to_bytes();

    // sha2 crate result
    let mut hasher = Sha256::new();
    hasher.update(left);
    hasher.update(right);
    let sha2_result: [u8; 32] = hasher.finalize().into();

    // solana syscall result
    let syscall_result = hashv(&[&left, &right]);

    assert_eq!(
        sha2_result,
        syscall_result.to_bytes(),
        "SHA256 syscall must match sha2 crate output"
    );

    // Also verify it matches ZEROS[1] (precomputed hash of PADDING_LEAF with itself)
    assert_eq!(
        sha2_result,
        ZEROS[1].to_bytes(),
        "Result must match precomputed ZEROS[1]"
    );
}

#[test]
fn test_padding_leaf_matches_arm_risc0() {
    // Verify PADDING_LEAF constant matches arm-risc0's value
    // Hex: cc1d2f838445db7aec431df9ee8a871f40e7aa5e064fc056633ef8c60fab7b06
    let expected_bytes =
        hex::decode("cc1d2f838445db7aec431df9ee8a871f40e7aa5e064fc056633ef8c60fab7b06").unwrap();
    let expected = Digest::from_bytes(expected_bytes.try_into().unwrap());
    assert_eq!(PADDING_LEAF, expected, "PADDING_LEAF must match arm-risc0");
}

#[test]
fn test_empty_tree_root_is_padding_based() {
    // Empty tree root at depth 1 should be PADDING_LEAF (= ZEROS[0])
    let state = create_test_pa_state();
    assert_eq!(state.next_index, 0);
    assert_eq!(state.current_depth, 1, "test state should start at depth 1");
    let root = compute_root_from_frontier(&state);
    // The empty root should NOT be all zeros - it's computed from padding
    assert_ne!(root, Digest::default());
    // Should equal ZEROS[depth - 1] = ZEROS[0] = PADDING_LEAF for depth 1
    assert_eq!(root, EMPTY_TREE_ROOT_INITIAL);
    assert_eq!(root, PADDING_LEAF);
}
