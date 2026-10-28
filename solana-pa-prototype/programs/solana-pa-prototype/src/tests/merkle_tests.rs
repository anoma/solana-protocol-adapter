use crate::merkle::{current_root, hash_two, push, EMPTY_TREE_ROOT_INITIAL, MAX_TREE_DEPTH, ZEROS};
use crate::state::PAStateAccount;
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
    let root = Digest::from_bytes(state.root);
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

/// Fails unless the adapter's tree is pa-evm's: the same leaf count, depth,
/// capacity, left siblings (what `commitmentTreeLeftSiblings` returns) and
/// root.
fn assert_tree_is_evms(state: &PAStateAccount, evm: &EvmMerkleTree, when: &str) {
    let frontier: Vec<Digest> = (0..state.frontier.len())
        .map(|level| state.get_frontier(level))
        .collect();
    assert_eq!(state.next_index, evm.leaf_count, "leaf count {when}");
    assert_eq!(state.depth(), evm.depth(), "depth {when}");
    assert_eq!(state.capacity(), 1u64 << evm.depth(), "capacity {when}");
    assert_eq!(frontier, evm.left_siblings, "left siblings {when}");
    assert_eq!(current_root(state), evm.current_root(), "root {when}");
}

/// The adapter's state holding pa-evm's tree `evm`.
fn state_holding(evm: &EvmMerkleTree) -> PAStateAccount {
    PAStateAccount {
        next_index: evm.leaf_count,
        current_depth: evm.depth() as u8,
        frontier: evm.left_siblings.iter().map(|&side| side.into()).collect(),
        ..create_test_pa_state()
    }
}

/// pa-evm's tree one leaf short of 2^`MAX_TREE_DEPTH` leaves: depth
/// `MAX_TREE_DEPTH`, with distinct left siblings.
fn evm_tree_one_leaf_short_of_full() -> EvmMerkleTree {
    EvmMerkleTree {
        leaf_count: (1u64 << MAX_TREE_DEPTH) - 1,
        left_siblings: (0..MAX_TREE_DEPTH)
            .map(|level| Digest::from_bytes([level as u8 + 1; 32]))
            .collect(),
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
        assert_tree_is_evms(&state, &evm, &format!("after {leaf_count} pushes"));

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

/// The 2^32nd leaf fills the depth-32 tree; as at every depth in pa-evm's
/// `push`, the full tree's root becomes the left sibling on a new level, and
/// the root is the depth-33 tree's.
#[test]
fn the_leaf_that_fills_the_deepest_tree_grows_it_as_evms() {
    let mut evm = evm_tree_one_leaf_short_of_full();
    let mut state = state_holding(&evm);
    let leaf = Digest::from_bytes([0x77; 32]);

    push(&mut state, leaf).unwrap();
    evm.push(leaf);

    println!(
        "after the 2^32nd leaf: count {}, depth {}, root {:?}",
        state.next_index,
        state.depth(),
        current_root(&state)
    );
    assert_tree_is_evms(&state, &evm, "after the 2^32nd leaf");
}

/// The tree holds at most 2^32 leaves: the next push is refused before any
/// mutation.
#[test]
fn the_leaf_after_the_two_to_the_thirty_second_is_refused_without_mutating() {
    let mut evm = evm_tree_one_leaf_short_of_full();
    evm.push(Digest::from_bytes([0x77; 32]));
    let mut state = state_holding(&evm);
    let before = state.clone();

    let result = push(&mut state, Digest::from_bytes([0x88; 32]));

    println!("push of the (2^32 + 1)th leaf: {result:?}");
    assert_anchor_err!(result, TreeMaxDepthReached);
    assert_eq!(
        state.next_index, before.next_index,
        "next_index must not change"
    );
    assert_eq!(
        state.current_depth, before.current_depth,
        "depth must not change"
    );
    assert_eq!(state.root, before.root, "root must not change");
    assert_eq!(state.frontier, before.frontier, "frontier must not change");
}
