use std::sync::Arc;

use anoma_pa_testkit::environment::StateBuilder;
use anoma_pa_testkit::prover::QueueProver;
use anoma_risc0_kind_tables::SolanaCluster;
use anoma_risc0_kind_tables::table;
use anoma_rm_risc0::compliance::KindTableEntry;
use anoma_rm_risc0::constants::{init_kind_table_from_entries, kind_table_hash};
use anyhow::Context;
use solana_keypair::Keypair;

use super::super::common::addresses::{DEVNET, program_id};
use super::Environment;
use super::config::E2eConfig;
use crate::state::cluster::Cluster;

impl Environment {
    pub async fn setup_bare() -> anyhow::Result<Self> {
        Self::setup(async |_| anyhow::Ok(())).await
    }

    pub async fn setup<F>(insert_additional: F) -> anyhow::Result<Self>
    where
        F: AsyncFnOnce(&mut StateBuilder) -> anyhow::Result<()>,
    {
        let config = E2eConfig::from_env().context("failed to parse e2e test config")?;

        // Fork devnet. The adapter is already deployed and initialized there,
        // so its address comes from the deployment record rather than a fresh
        // deployment; forking keeps devnet's state from being mutated.
        let payer = Arc::new(Keypair::new());
        let surfnet = super::super::common::runtime::builder(&payer)
            .offline(false)
            .remote_rpc_url(config.devnet_rpc_url.clone())
            .start()
            .await
            .context("failed to start the surfpool runtime forking devnet")?;
        let pa = program_id(DEVNET, "PROTOCOL_ADAPTER")?;
        let prover = QueueProver::new(&config.queue_base_url, &config.queue_auth_token)
            .context("failed to build queue prover")?;

        let env = Self::assemble(
            surfnet,
            Cluster::Devnet,
            payer,
            pa,
            prover,
            insert_additional,
        )
        .await?;
        load_kind_table(env.protocol_adapter.state().await?.kind_table_commitment)?;
        Ok(env)
    }
}

/// Loads the kind table recorded for devnet, and checks that the adapter
/// stores its commitment.
fn load_kind_table(stored: [u8; 32]) -> anyhow::Result<()> {
    let entries = table::staging::table(SolanaCluster::Devnet)
        .context("no kind table is recorded for solana-devnet")?
        .entries
        .iter()
        .map(KindTableEntry::from)
        .collect();
    init_kind_table_from_entries(entries).context("failed to load the kind table")?;
    let loaded = kind_table_hash().context("no kind table loaded")?;
    anyhow::ensure!(
        stored == loaded.as_bytes(),
        "the protocol adapter stores the kind table {}, the tests prove against {loaded}",
        anoma_rm_risc0::Digest::from_bytes(stored)
    );
    Ok(())
}
