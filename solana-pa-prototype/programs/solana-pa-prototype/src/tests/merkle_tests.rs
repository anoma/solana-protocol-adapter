use crate::compute_root_from_frontier;
use crate::merkle::{EMPTY_TREE_ROOT_INITIAL, PADDING_LEAF, ZEROS};
use crate::tests::utils::create_test_pa_state;
use crate::types::Digest;
use anchor_lang::solana_program::hash::hashv;
use sha2::{Digest as Sha2Digest, Sha256};

/// Switching from sha2 crate to syscall must not break arm-risc0 merkle tree compatibility.
#[test]
fn test_sha256_syscall_matches_sha2_crate() {
    let left = PADDING_LEAF.to_bytes();
    let right = PADDING_LEAF.to_bytes();

    let mut hasher = Sha256::new();
    hasher.update(left);
    hasher.update(right);
    let sha2_result: [u8; 32] = hasher.finalize().into();

    let syscall_result = hashv(&[&left, &right]);

    assert_eq!(
        sha2_result,
        syscall_result.to_bytes(),
        "SHA256 syscall must match sha2 crate output"
    );

    assert_eq!(
        sha2_result,
        ZEROS[1].to_bytes(),
        "Result must match precomputed ZEROS[1]"
    );
}

#[test]
fn test_padding_leaf_matches_arm_risc0() {
    let expected_bytes: [u8; 32] =
        hex_literal::hex!("cc1d2f838445db7aec431df9ee8a871f40e7aa5e064fc056633ef8c60fab7b06");
    let expected = Digest::from_bytes(expected_bytes);
    assert_eq!(PADDING_LEAF, expected, "PADDING_LEAF must match arm-risc0");
}

#[test]
fn test_empty_tree_root_is_padding_based() {
    let state = create_test_pa_state();
    assert_eq!(state.next_index, 0);
    assert_eq!(state.current_depth, 1, "test state should start at depth 1");
    let root = compute_root_from_frontier(&state);
    assert_ne!(
        root,
        Digest::default(),
        "empty root is computed from padding, not zeros"
    );
    assert_eq!(root, EMPTY_TREE_ROOT_INITIAL);
    assert_eq!(root, PADDING_LEAF);
}
