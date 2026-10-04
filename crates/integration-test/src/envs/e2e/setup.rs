use std::sync::Arc;

use anoma_pa_testkit::environment::StateBuilder;
use anoma_pa_testkit::prover::QueueProver;
use anyhow::Context;
use solana_keypair::Keypair;

use super::super::common::addresses::{DEVNET, program_id};
use super::super::common::runtime;
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
        let surfnet = runtime::start(&payer, |builder| {
            builder.remote_rpc_url(config.devnet_rpc_url.clone())
        })
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
        // The tests prove against devnet's kind table, which the adapter must
        // store.
        let loaded = crate::kind_table::load_devnet()?;
        let stored = env.protocol_adapter.state().await?.kind_table_commitment;
        anyhow::ensure!(
            stored == loaded,
            "the protocol adapter stores the kind table {}, the tests prove against {}",
            anoma_rm_risc0::Digest::from_bytes(stored),
            anoma_rm_risc0::Digest::from_bytes(loaded)
        );
        Ok(env)
    }
}
