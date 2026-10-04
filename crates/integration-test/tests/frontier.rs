//! The adapter's stored frontier is the sides pa-testkit's
//! `FrontierCommitmentTree` starts from: for every count of leaves the
//! adapter's tree held, the tree built from its frontier gives the adapter's
//! root, before and after the leaves a settlement adds.

use anoma_pa_solana_client::CommitmentTreeState;
use anoma_pa_testkit::commitment_tree::{FrontierCommitmentTree, depth_at};
use anoma_pa_testkit::environment::CommitmentTree;
use anoma_rm_risc0::Digest;

fn leaf(seed: usize) -> [u8; 32] {
    let mut leaf = [0u8; 32];
    leaf[..8].copy_from_slice(&(seed as u64 + 1).to_le_bytes());
    leaf
}

#[test]
fn the_tree_from_the_adapters_frontier_gives_the_adapters_roots() {
    for read in 0..=33 {
        let leaves: Vec<[u8; 32]> = (0..read).map(leaf).collect();
        let mut adapter = CommitmentTreeState::over(&leaves).unwrap();
        let sides = adapter.frontier[..depth_at(read)]
            .iter()
            .map(|side| Digest::from_bytes(*side))
            .collect();
        let mut tree = FrontierCommitmentTree::new(read, sides).unwrap();
        assert_eq!(
            tree.root().unwrap().as_bytes(),
            adapter.root,
            "{read} leaves read"
        );
        for added in read..read + 9 {
            adapter.append(leaf(added)).unwrap();
            tree.add([Digest::from_bytes(leaf(added))]);
            assert_eq!(
                tree.root().unwrap().as_bytes(),
                adapter.root,
                "{read} leaves read, {} added",
                added + 1 - read
            );
        }
    }
}
