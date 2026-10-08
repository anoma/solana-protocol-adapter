//! The adapter's commitment tree as pa-testkit's `FrontierCommitmentTree`.

use anoma_pa_solana_client::PAStateAccount;
use anoma_pa_testkit::commitment_tree::{FrontierCommitmentTree, depth_at};
use anoma_rm_risc0::Digest;
use anyhow::Context;

/// The tree the adapter's state holds: its commitment count and, per level of
/// the tree at that count, the frontier node. The adapter's tree starts at
/// depth 1, so its empty tree stores one frontier node the testkit's (depth 0)
/// does not read.
pub fn from_state(state: &PAStateAccount) -> anyhow::Result<FrontierCommitmentTree> {
    let count = usize::try_from(state.next_index).context("the commitment count exceeds usize")?;
    let sides = state
        .frontier
        .get(..depth_at(count))
        .with_context(|| {
            format!(
                "the adapter stores {} frontier nodes at {count} leaves",
                state.frontier.len()
            )
        })?
        .iter()
        .map(|side| Digest::from_bytes(*side))
        .collect();
    FrontierCommitmentTree::new(count, sides)
}
