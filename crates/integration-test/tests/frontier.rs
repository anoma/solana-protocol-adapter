//! The adapter's stored frontier is the sides pa-testkit's
//! `FrontierCommitmentTree` starts from: for every count of leaves the
//! adapter's tree held, the tree `commitment_tree::from_state` builds gives
//! the adapter's root, before and after the leaves a settlement adds.

use anoma_pa_solana_client::{CommitmentTreeState, PAStateAccount};
use anoma_pa_solana_integration_test::commitment_tree::from_state;
use anoma_rm_risc0::Digest;

fn leaf(seed: usize) -> [u8; 32] {
    let mut leaf = [0u8; 32];
    leaf[..8].copy_from_slice(&(seed as u64 + 1).to_le_bytes());
    leaf
}

/// An adapter state holding `tree`; the fields outside the tree are
/// irrelevant to it.
fn state_holding(tree: &CommitmentTreeState) -> PAStateAccount {
    PAStateAccount {
        schema_version: 4,
        bump: 255,
        owner: [1; 32],
        verifier_router: [2; 32],
        proof_selector: [0xff; 4],
        kind_table_commitment: [0; 32],
        paused: false,
        root: tree.root,
        next_index: tree.next_index,
        current_depth: tree.current_depth,
        frontier: tree.frontier.clone(),
        min_expiry_slots: 10,
        max_expiry_slots: 1000,
        denied_consumed_logic_refs: vec![],
        denied_created_logic_refs: vec![],
    }
}

#[test]
fn the_tree_from_the_adapters_state_gives_the_adapters_roots() {
    for read in 0..=33 {
        let leaves: Vec<[u8; 32]> = (0..read).map(leaf).collect();
        let mut adapter = CommitmentTreeState::over(&leaves).unwrap();
        let mut tree = from_state(&state_holding(&adapter)).unwrap();
        assert_eq!(tree.root().as_bytes(), adapter.root, "{read} leaves read");
        for added in read..read + 9 {
            adapter.append(leaf(added)).unwrap();
            tree.add([Digest::from_bytes(leaf(added))]);
            assert_eq!(
                tree.root().as_bytes(),
                adapter.root,
                "{read} leaves read, {} added",
                added + 1 - read
            );
        }
    }
}
