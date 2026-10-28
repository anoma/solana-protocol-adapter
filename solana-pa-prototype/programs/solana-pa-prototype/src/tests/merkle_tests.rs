use crate::merkle::{current_root, hash_two, push, EMPTY_TREE_ROOT_INITIAL, ZEROS};
use crate::tests::utils::{assert_anchor_err, create_test_pa_state};
use arm_core::merkle_path::PADDING_LEAF;
use arm_core::Digest;
use sha2::{Digest as Sha2Digest, Sha256};
use solana_sha256_hasher::hashv;

/// Switching from sha2 crate to syscall must not break arm-risc0 merkle tree compatibility.
/// The cross-check against ZEROS[1] also confirms the precomputed table uses the same hash impl.
#[test]
fn test_sha256_syscall_matches_sha2_crate() {
    let left: [u8; 32] = PADDING_LEAF.into();
    let right: [u8; 32] = PADDING_LEAF.into();

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
        <[u8; 32]>::from(ZEROS[1]),
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
    assert_eq!(
        state.current_depth, 0,
        "the empty tree has depth 0, as pa-evm's"
    );
    let root = state.root_digest();
    assert_ne!(
        root,
        Digest::default(),
        "empty root is computed from padding, not zeros"
    );
    assert_eq!(root, EMPTY_TREE_ROOT_INITIAL);
    assert_eq!(root, PADDING_LEAF);
}

/// pa-evm V2's `MerkleTree` (`contracts/src/libs/MerkleTree.sol` on `next`):
/// `setup`, `push` and `currentRoot`, line for line.
struct EvmMerkleTree {
    leaf_count: u64,
    left_siblings: Vec<Digest>,
}

impl EvmMerkleTree {
    fn setup() -> Self {
        Self {
            leaf_count: 0,
            left_siblings: Vec::new(),
        }
    }

    fn depth(&self) -> usize {
        self.left_siblings.len()
    }

    fn push(&mut self, leaf: Digest) {
        let leaf_index = self.leaf_count;
        self.leaf_count += 1;
        let mut node = leaf;
        let mut level = 0;
        let mut node_index = leaf_index;
        while node_index & 1 == 1 {
            node = hash_two(&self.left_siblings[level], &node);
            level += 1;
            node_index >>= 1;
        }
        if level == self.depth() {
            self.left_siblings.push(node);
        } else {
            self.left_siblings[level] = node;
        }
    }

    fn current_root(&self) -> Digest {
        let mut node_index = self.leaf_count;
        let mut empty_subtree_root = PADDING_LEAF;
        let mut node = empty_subtree_root;
        for level in 0..self.depth() {
            node = if node_index & 1 == 1 {
                hash_two(&self.left_siblings[level], &node)
            } else {
                hash_two(&node, &empty_subtree_root)
            };
            empty_subtree_root = hash_two(&empty_subtree_root, &empty_subtree_root);
            node_index >>= 1;
        }
        node
    }
}

/// The commitment tree is pa-evm V2's after every push, starting from the
/// empty tree: the same depth, capacity, left siblings (what
/// `commitmentTreeLeftSiblings` returns) and root.
#[test]
fn tree_matches_evm_merkle_tree_from_empty_through_every_push() {
    let mut state = create_test_pa_state();
    let mut evm = EvmMerkleTree::setup();
    for leaf_count in 0..=70u64 {
        let frontier: Vec<Digest> = (0..state.frontier.len())
            .map(|level| state.get_frontier(level))
            .collect();
        assert_eq!(
            state.next_index, evm.leaf_count,
            "leaf count after {leaf_count} pushes"
        );
        assert_eq!(
            state.depth(),
            evm.depth(),
            "depth after {leaf_count} pushes"
        );
        assert_eq!(
            state.capacity(),
            1u64 << evm.depth(),
            "capacity after {leaf_count} pushes"
        );
        assert_eq!(
            frontier, evm.left_siblings,
            "left siblings after {leaf_count} pushes"
        );
        assert_eq!(
            current_root(&state),
            evm.current_root(),
            "root after {leaf_count} pushes"
        );

        let leaf = Digest::from_bytes([leaf_count as u8 + 1; 32]);
        push(&mut state, leaf).unwrap();
        evm.push(leaf);
    }
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

/// Regression test: the incremental `push` and `current_root` must produce
/// the same root as the EVM's batch `computeRoot` for 1..8 leaves.
/// This is the primary guard against the expand-after-fill bug
/// recurring: if `push` ever fails to expand when exactly
/// full, the roots will diverge at every power-of-two leaf count.
#[test]
fn test_incremental_matches_evm_compute_root() {
    let leaves: Vec<Digest> = (1u8..=8).map(|i| Digest::from_bytes([i; 32])).collect();

    let mut state = create_test_pa_state();
    for (i, leaf) in leaves.iter().enumerate() {
        push(&mut state, *leaf).unwrap();
        let incremental_root = current_root(&state);

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

/// A depth-32 tree declares capacity 2^32 and must accept index 2^32 - 1.
#[test]
fn test_append_accepts_final_declared_leaf_at_max_depth() {
    let mut state = create_test_pa_state();
    state.current_depth = 32;
    state.frontier = vec![[0u8; 32]; 32];
    state.next_index = (1u64 << 32) - 1;

    let leaf = Digest::from_bytes([0x77; 32]);
    let result = push(&mut state, leaf);

    assert!(
        result.is_ok(),
        "index 2^32-1 is within declared capacity and must be accepted, got {:?}",
        result
    );
    assert_eq!(state.next_index, 1u64 << 32, "final leaf must be committed");
    assert_eq!(
        state.current_depth, 32,
        "tree must not expand past max depth"
    );

    // index = 2^32 - 1 is all-ones in binary, so every one of the 32 levels
    // takes the "right child" branch, hashing the (test) all-zero frontier
    // against the running value: the full depth-32 tree's root. pa-evm's
    // push then grows the tree to depth 33, whose root pads that with the
    // empty depth-32 subtree (`MerkleTree.push`, `currentRoot`).
    // The account cannot hold a 33rd level, so the next push is refused, but
    // the root this push adds is pa-evm's.
    let zero_frontier = Digest::from_bytes([0u8; 32]);
    let mut full = leaf;
    for _ in 0..32 {
        full = hash_two(&zero_frontier, &full);
    }
    let empty_depth_32 = hash_two(&ZEROS[31], &ZEROS[31]);
    assert_eq!(
        current_root(&state),
        hash_two(&full, &empty_depth_32),
        "the root of 2^32 leaves must be pa-evm's depth-33 root"
    );
}

/// Once full at max depth, the next append is rejected before any mutation.
#[test]
fn test_append_rejects_when_full_without_mutating() {
    let mut state = create_test_pa_state();
    state.current_depth = 32;
    state.frontier = vec![[0u8; 32]; 32];
    state.next_index = 1u64 << 32;
    state.root = [0xAB; 32];

    let before_index = state.next_index;
    let before_depth = state.current_depth;
    let before_root = state.root;
    let before_frontier = state.frontier.clone();

    let result = push(&mut state, Digest::from_bytes([0x88; 32]));

    assert_anchor_err!(result, TreeMaxDepthReached);
    assert_eq!(state.next_index, before_index, "next_index must not change");
    assert_eq!(state.current_depth, before_depth, "depth must not change");
    assert_eq!(state.root, before_root, "root must not change");
    assert_eq!(state.frontier, before_frontier, "frontier must not change");
}
