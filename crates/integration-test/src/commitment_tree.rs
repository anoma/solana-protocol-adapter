//! The adapter's commitment tree as pa-testkit's `FrontierCommitmentTree`.

use anoma_pa_solana_client::generated::protocol_adapter::accounts::PAStateAccount;
use anoma_pa_testkit::commitment_tree::FrontierCommitmentTree;
use anoma_rm_risc0::Digest;
use anyhow::Context;

/// The tree the adapter's state holds: its commitment count and its frontier,
/// one node per level of the tree at that count.
pub fn from_state(state: &PAStateAccount) -> anyhow::Result<FrontierCommitmentTree> {
    let count = usize::try_from(state.next_index).context("the commitment count exceeds usize")?;
    let sides = state
        .frontier
        .iter()
        .map(|side| Digest::from_bytes(*side))
        .collect();
    FrontierCommitmentTree::new(count, sides)
}
