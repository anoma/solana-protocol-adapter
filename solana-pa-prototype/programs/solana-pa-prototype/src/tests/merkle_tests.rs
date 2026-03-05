use crate::merkle::{append_to_tree, compute_root_from_frontier};
use crate::merkle::{hash_two, EMPTY_TREE_ROOT_INITIAL, PADDING_LEAF, ZEROS};
use crate::tests::utils::create_test_pa_state;
use anchor_lang::solana_program::hash::hashv;
use arm_core::Digest;
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

#[test]
fn test_zeros_chain_property() {
    assert_eq!(ZEROS[0], PADDING_LEAF);
    for i in 1..ZEROS.len() {
        assert_eq!(
            ZEROS[i],
            hash_two(&ZEROS[i - 1], &ZEROS[i - 1]),
            "ZEROS[{}] must equal hash(ZEROS[{}], ZEROS[{}])",
            i,
            i - 1,
            i - 1
        );
    }
}

#[test]
fn test_full_tree_root_at_depth_1() {
    let mut state = create_test_pa_state();
    let leaf0 = Digest::from_bytes([0x01; 32]);
    let leaf1 = Digest::from_bytes([0x02; 32]);

    append_to_tree(&mut state, leaf0).unwrap();
    append_to_tree(&mut state, leaf1).unwrap();

    assert_eq!(state.next_index, 2);
    assert_eq!(state.current_depth, 1);

    let root = compute_root_from_frontier(&state);
    let expected = hash_two(&leaf0, &leaf1);
    assert_eq!(
        root, expected,
        "full depth-1 tree root must be hash(leaf0, leaf1), not ZEROS[1]"
    );
}

#[test]
fn test_root_preserved_across_growth() {
    let mut state = create_test_pa_state();
    let leaf0 = Digest::from_bytes([0x01; 32]);
    let leaf1 = Digest::from_bytes([0x02; 32]);
    let leaf2 = Digest::from_bytes([0x03; 32]);

    append_to_tree(&mut state, leaf0).unwrap();
    append_to_tree(&mut state, leaf1).unwrap();
    // Tree is full at depth 1 (capacity 2). Third append triggers growth.
    append_to_tree(&mut state, leaf2).unwrap();

    assert_eq!(state.current_depth, 2, "tree should have grown to depth 2");
    assert_eq!(state.next_index, 3);

    let root = compute_root_from_frontier(&state);
    // Depth-2 tree: left subtree = hash(leaf0, leaf1), right subtree = hash(leaf2, PADDING)
    let left = hash_two(&leaf0, &leaf1);
    let right = hash_two(&leaf2, &PADDING_LEAF);
    let expected = hash_two(&left, &right);
    assert_eq!(
        root, expected,
        "root after growth must preserve leaves from the full subtree"
    );
}
