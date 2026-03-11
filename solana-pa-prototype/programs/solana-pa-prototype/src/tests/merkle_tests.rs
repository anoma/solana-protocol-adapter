use crate::merkle::{append_to_tree, hash_two, EMPTY_TREE_ROOT_INITIAL, PADDING_LEAF, ZEROS};
use crate::tests::utils::create_test_pa_state;
use anchor_lang::solana_program::hash::hashv;
use arm_core::Digest;
use sha2::{Digest as Sha2Digest, Sha256};

/// Switching from sha2 crate to syscall must not break arm-risc0 merkle tree compatibility.
/// The cross-check against ZEROS[1] also confirms the precomputed table uses the same hash impl.
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
        "ZEROS[1] must match the syscall/sha2 output, proving precomputed table uses the same hash"
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
    let root = state.root_digest();
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

/// EVM MerkleTree.sol expand-after-fill: when the tree is exactly full,
/// it expands by one level. A tree with 2 leaves (filled depth 1) has
/// depth 2 and root = hash(hash(L0, L1), ZEROS[1]).
#[test]
fn test_expand_after_fill_at_depth_1() {
    let mut state = create_test_pa_state();
    let leaf0 = Digest::from_bytes([0x01; 32]);
    let leaf1 = Digest::from_bytes([0x02; 32]);

    append_to_tree(&mut state, leaf0).unwrap();
    append_to_tree(&mut state, leaf1).unwrap();

    assert_eq!(state.next_index, 2);
    assert_eq!(
        state.current_depth, 2,
        "filling depth 1 must expand to depth 2"
    );

    let root = state.root_digest();
    let expected = hash_two(&hash_two(&leaf0, &leaf1), &ZEROS[1]);
    assert_eq!(
        root, expected,
        "root must include zero-padding at the expanded level"
    );
}

/// After expand-after-fill grows the tree to depth 2, the 3rd leaf
/// goes into the right subtree without triggering another expansion.
#[test]
fn test_append_after_expansion() {
    let mut state = create_test_pa_state();
    let leaf0 = Digest::from_bytes([0x01; 32]);
    let leaf1 = Digest::from_bytes([0x02; 32]);
    let leaf2 = Digest::from_bytes([0x03; 32]);

    append_to_tree(&mut state, leaf0).unwrap();
    append_to_tree(&mut state, leaf1).unwrap();
    append_to_tree(&mut state, leaf2).unwrap();

    assert_eq!(state.current_depth, 2, "depth should remain 2");
    assert_eq!(state.next_index, 3);

    let root = state.root_digest();
    let left = hash_two(&leaf0, &leaf1);
    let right = hash_two(&leaf2, &PADDING_LEAF);
    let expected = hash_two(&left, &right);
    assert_eq!(root, expected, "root after 3 leaves at depth 2");
}

/// Port of MerkleTree.sol `computeRoot`: builds a full tree of `2^depth`
/// nodes from the given leaves (padding with PADDING_LEAF), then hashes
/// upward to produce the root. Used as the reference oracle in tests.
fn evm_compute_root(leaves: &[Digest], tree_depth: usize) -> Digest {
    let capacity = 1usize << tree_depth;
    let mut nodes: Vec<Digest> = (0..capacity)
        .map(|i| {
            if i < leaves.len() {
                leaves[i]
            } else {
                PADDING_LEAF
            }
        })
        .collect();
    let mut width = capacity;
    while width > 1 {
        width /= 2;
        for i in 0..width {
            nodes[i] = hash_two(&nodes[2 * i], &nodes[2 * i + 1]);
        }
    }
    nodes[0]
}

/// Port of MerkleTree.sol `computeMinimalTreeDepth`.
fn evm_minimal_depth(leaf_count: usize) -> usize {
    if leaf_count == 0 {
        return 0;
    }
    let bits = usize::BITS - (leaf_count - 1).leading_zeros();
    bits as usize
}

/// Regression test: the incremental `append_to_tree` must produce
/// the same root as the EVM's batch `computeRoot` for 1..8 leaves.
/// This is the primary guard against the expand-after-fill bug
/// recurring: if `append_to_tree` ever fails to expand when exactly
/// full, the roots will diverge at every power-of-two leaf count.
#[test]
fn test_incremental_matches_evm_compute_root() {
    let leaves: Vec<Digest> = (1u8..=8).map(|i| Digest::from_bytes([i; 32])).collect();

    let mut state = create_test_pa_state();
    for (i, leaf) in leaves.iter().enumerate() {
        append_to_tree(&mut state, *leaf).unwrap();
        let incremental_root = state.root_digest();

        let n = i + 1;
        // The EVM tree depth after N pushes: for non-power-of-two N it's
        // ceil(log2(N)); for power-of-two N it's log2(N)+1 (expanded).
        let evm_depth = if n.is_power_of_two() {
            n.trailing_zeros() as usize + 1
        } else {
            evm_minimal_depth(n)
        };
        let batch_root = evm_compute_root(&leaves[..n], evm_depth);

        assert_eq!(
            incremental_root,
            batch_root,
            "root mismatch after {} leaves: incremental depth={}, evm depth={}",
            n,
            state.depth(),
            evm_depth,
        );
        assert_eq!(
            state.depth(),
            evm_depth,
            "depth mismatch after {} leaves",
            n,
        );
    }
}

/// Verify that filling a depth-2 tree (4 leaves) expands to depth 3.
/// This is the exact scenario that triggered the original bug.
#[test]
fn test_expand_after_fill_at_depth_2() {
    let mut state = create_test_pa_state();
    let leaves: Vec<Digest> = (1u8..=4).map(|i| Digest::from_bytes([i; 32])).collect();

    for leaf in &leaves {
        append_to_tree(&mut state, *leaf).unwrap();
    }

    assert_eq!(state.next_index, 4);
    assert_eq!(
        state.current_depth, 3,
        "filling depth 2 must expand to depth 3"
    );

    let root = state.root_digest();
    let expected = evm_compute_root(&leaves, 3);
    assert_eq!(root, expected, "4-leaf root must match EVM at depth 3");
}
