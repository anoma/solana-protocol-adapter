//! The adapter's stored frontier is the sides pa-testkit's
//! `FrontierCommitmentTree` starts from: for every count of leaves the
//! adapter's tree held, the tree `commitment_tree::from_state` builds gives
//! the adapter's root, before and after the leaves a settlement adds. And the
//! client's replay of the settled commitments is the adapter's tree.

use anoma_pa_solana_client::CommitmentTreeState;
use anoma_pa_solana_client::generated::protocol_adapter::accounts::PAStateAccount;
use anoma_pa_solana_client::generated::protocol_adapter::constants::SCHEMA_VERSION;
use anoma_pa_solana_integration_test::commitment_tree::from_state;
use anoma_pa_solana_integration_test::envs::local::Environment as SolanaLocalEnv;
use anoma_pa_testkit::fixtures::ordered::{self, Logic, ResourceSpec};
use anoma_pa_testkit::{execute_tx, prove_actions};
use anoma_rm_risc0::Digest;
use surfpool_sdk::Pubkey;

fn leaf(seed: usize) -> [u8; 32] {
    let mut leaf = [0u8; 32];
    leaf[..8].copy_from_slice(&(seed as u64 + 1).to_le_bytes());
    leaf
}

/// An adapter state holding `tree`; the fields outside the tree are
/// irrelevant to it.
fn state_holding(tree: &CommitmentTreeState) -> PAStateAccount {
    PAStateAccount {
        schema_version: SCHEMA_VERSION,
        bump: 255,
        owner: Pubkey::new_from_array([1; 32]),
        verifier_router: Pubkey::new_from_array([2; 32]),
        proof_selector: [0xff; 4],
        kind_tree_root: [0; 32],
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

/// Fails unless the adapter's tree is the one the client's
/// `CommitmentTreeState` replays from `leaves`: its count, depth, frontier
/// and root.
async fn ensure_the_replay_is_the_adapters_tree(
    env: &SolanaLocalEnv,
    leaves: &[[u8; 32]],
) -> anyhow::Result<()> {
    let chain = env.protocol_adapter.state().await?;
    let replay = CommitmentTreeState::over(leaves)?;
    let held = (
        chain.next_index,
        chain.current_depth,
        &chain.frontier,
        chain.root,
    );
    let replayed = (
        replay.next_index,
        replay.current_depth,
        &replay.frontier,
        replay.root,
    );
    anyhow::ensure!(
        held == replayed,
        "after {} leaves the adapter holds the tree (count, depth, frontier, root) {held:02x?}, \
         the client replays {replayed:02x?}",
        leaves.len()
    );
    Ok(())
}

// The tree the client's `CommitmentTreeState` replays from the commitments
// the settled transactions created, in their list order, is the adapter's:
// on the empty tree (depth 0), then after settlements that leave 1, 3, 5, 7
// and 9 leaves. The first action is created, consumed; the others are
// created, consumed, created.
#[tokio::test(flavor = "multi_thread")]
async fn the_clients_replay_of_the_settled_commitments_is_the_adapters_tree() -> anyhow::Result<()>
{
    let mut env = SolanaLocalEnv::setup_bare().await?;
    let mut leaves: Vec<[u8; 32]> = Vec::new();
    ensure_the_replay_is_the_adapters_tree(&env, &leaves).await?;
    let resource = |consumed| ResourceSpec {
        consumed,
        logic: Logic::Trivial,
    };
    for seed in 1..=5 {
        let shape = if seed == 1 {
            vec![resource(false), resource(true)]
        } else {
            vec![resource(false), resource(true), resource(false)]
        };
        let action = ordered::build(seed, shape, ordered::Overrides::default())?;
        let tx = prove_actions(&env, &[action.witnesses]).await?;
        leaves.extend(tx.created_commitments()?.map(<[u8; 32]>::from));
        execute_tx(&mut env, tx).await?;
        ensure_the_replay_is_the_adapters_tree(&env, &leaves).await?;
    }
    Ok(())
}
