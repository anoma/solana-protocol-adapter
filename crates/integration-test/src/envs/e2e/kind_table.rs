//! The kind table the e2e environment proves against: solana-devnet's. The
//! prover's kind table is process-wide and set once.

use anoma_risc0_kind_tables::{SolanaCluster, table};
use anoma_rm_risc0::compliance::{KindTableEntry, hash_kind_table_entries};
use anoma_rm_risc0::constants::{init_kind_table_from_entries, kind_table_hash};
use anyhow::Context;

/// Makes solana-devnet's kind table, as anoma/risc0-kind-tables records it,
/// the prover's, and returns its commitment. Fails when the process's prover
/// already holds another table.
pub(super) fn load_devnet() -> anyhow::Result<[u8; 32]> {
    let entries: Vec<KindTableEntry> = table::staging::table(SolanaCluster::Devnet)
        .context("no kind table is recorded for solana-devnet")?
        .entries
        .iter()
        .map(KindTableEntry::from)
        .collect();
    let devnet = hash_kind_table_entries(&entries);
    init_kind_table_from_entries(entries).context("failed to load the kind table")?;
    let loaded = *kind_table_hash().context("no kind table loaded")?;
    anyhow::ensure!(
        loaded == devnet,
        "the prover already holds the kind table {loaded}, not solana-devnet's {devnet}"
    );
    Ok(devnet.into())
}
