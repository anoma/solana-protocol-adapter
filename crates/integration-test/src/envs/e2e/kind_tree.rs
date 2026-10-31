//! The kind tree the e2e environment proves against: solana-devnet's. The
//! prover's kind tree is process-wide and set once.

use anoma_risc0_kind_trees::{SolanaCluster, tree};
use anoma_rm_risc0::compliance::KindTreeEntry;
use anoma_rm_risc0::constants::{init_kind_tree_from_entries, kind_tree_root};
use anyhow::Context;

/// Makes solana-devnet's kind tree, as anoma/risc0-kind-trees records it,
/// the prover's, and returns the root it records for it. Fails when the
/// process's prover already holds another tree, or the recorded entries do
/// not have the recorded root.
pub(super) fn load_devnet() -> anyhow::Result<[u8; 32]> {
    let entries: Vec<KindTreeEntry> = tree::staging::tree(SolanaCluster::Devnet)
        .context("no kind tree is recorded for solana-devnet")?
        .entries
        .iter()
        .map(KindTreeEntry::from)
        .collect();
    let devnet = tree::staging::root(SolanaCluster::Devnet)
        .context("no kind tree root is recorded for solana-devnet")?;
    init_kind_tree_from_entries(entries).context("failed to load the kind tree")?;
    let loaded = kind_tree_root().context("no kind tree loaded")?;
    anyhow::ensure!(
        loaded == devnet,
        "the prover already holds the kind tree {loaded}, not solana-devnet's {devnet}"
    );
    Ok(devnet.into())
}
