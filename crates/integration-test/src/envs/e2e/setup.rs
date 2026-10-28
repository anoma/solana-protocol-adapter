use std::sync::Arc;

use anoma_pa_testkit::prover::{QueueProver, Risc0Prover};
use anyhow::Context;
use solana_keypair::Keypair;
use solana_signer::Signer;

use super::super::common::addresses::{DEVNET, program_id};
use super::super::common::deployment::{self, Verifier};
use super::super::common::environment::JOURNAL_ENCODING;
use super::super::common::runtime;
use super::config::{E2eConfig, E2eRuntime};
use super::{Environment, Prover};

impl Environment {
    /// The environment the process environment configures (`E2eConfig`).
    pub async fn setup_bare() -> anyhow::Result<Self> {
        let config = E2eConfig::from_env().context("failed to parse e2e test config")?;
        Self::setup(&config).await
    }

    /// The environment `config` describes.
    pub async fn setup(config: &E2eConfig) -> anyhow::Result<Self> {
        let prover = match &config.queue {
            Some(queue) => Prover::Queue(
                QueueProver::new(&queue.base_url, &queue.auth_token, JOURNAL_ENCODING)
                    .context("failed to build queue prover")?,
            ),
            None => Prover::Risc0(Risc0Prover::new(JOURNAL_ENCODING)),
        };
        match &config.runtime {
            E2eRuntime::Local => {
                let payer = Arc::new(Keypair::new());
                let surfnet = runtime::start(&payer, |builder| builder)
                    .await
                    .context("failed to start the surfpool runtime")?;
                let pa = deployment::deploy(&surfnet, &payer, Verifier::Groth16).await?;
                let rpc = runtime::client(surfnet.rpc_url().to_string());
                Self::assemble(Some(surfnet), rpc, payer, pa, prover).await
            }
            E2eRuntime::Devnet { rpc_url } => {
                // The adapter is already deployed and initialized on devnet,
                // so its address comes from the deployment record rather than
                // a fresh deployment; forking keeps devnet's state from being
                // mutated.
                let payer = Arc::new(Keypair::new());
                let surfnet =
                    runtime::start(&payer, |builder| builder.remote_rpc_url(rpc_url.clone()))
                        .await
                        .context("failed to start the surfpool runtime forking devnet")?;
                let pa = program_id(DEVNET, "PROTOCOL_ADAPTER")?;
                let owner = payer.pubkey();
                let rpc = runtime::client(surfnet.rpc_url().to_string());
                let env = Self::assemble(Some(surfnet), rpc, payer, pa, prover).await?;
                // The suite makes the owner's calls as the payer, so on the
                // fork the payer owns the adapter.
                env.take_ownership(owner).await?;
                env.ensure_devnet_kind_table().await?;
                Ok(env)
            }
            E2eRuntime::Cluster { rpc_url, payer } => {
                // The adapter is deployed and initialized on the cluster, at
                // the address the deployment record names; the tests send
                // their transactions to the cluster, which keeps them.
                let payer = solana_keypair::read_keypair_file(payer).map_err(|error| {
                    anyhow::anyhow!("failed to read the payer {}: {error}", payer.display())
                })?;
                let pa = program_id(DEVNET, "PROTOCOL_ADAPTER")?;
                let rpc = runtime::client(rpc_url.clone());
                let env = Self::assemble(None, rpc, Arc::new(payer), pa, prover).await?;
                env.ensure_devnet_kind_table().await?;
                Ok(env)
            }
        }
    }

    /// Makes solana-devnet's kind table the prover's, and checks that the
    /// adapter stores its root: the tests prove against devnet's kind table.
    async fn ensure_devnet_kind_table(&self) -> anyhow::Result<()> {
        let loaded = super::kind_table::load_devnet()?;
        let stored = self.protocol_adapter.state().await?.kind_tree_root;
        anyhow::ensure!(
            stored == loaded,
            "the protocol adapter stores the kind table {}, the tests prove against {}",
            anoma_rm_risc0::Digest::from_bytes(stored),
            anoma_rm_risc0::Digest::from_bytes(loaded)
        );
        Ok(())
    }
}
