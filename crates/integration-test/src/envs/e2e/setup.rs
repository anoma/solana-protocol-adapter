use std::sync::Arc;

use anoma_pa_solana_client::deployments::{chain_id, chain_id_of_genesis_hash};
use anoma_pa_testkit::prover::{QueueProver, Risc0Prover};
use anyhow::Context;
use solana_cluster_type::ClusterType;
use solana_keypair::Keypair;
use solana_signer::Signer;

use super::super::common::addresses::{DEVNET, program_id};
use super::super::common::deployment::{self, Verifier};
use super::super::common::environment::JOURNAL_ENCODING;
use super::super::common::runtime;
use super::config::{E2eConfig, E2eRuntime};
use super::{Environment, Prover};

impl Environment {
    /// The environment the process environment configures (`E2eConfig`), for
    /// the harness's suite, which refuses the cluster runtime.
    pub async fn setup_bare() -> anyhow::Result<Self> {
        let config = E2eConfig::from_env().context("failed to parse e2e test config")?;
        config.ensure_harness_suite_runtime()?;
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
                env.ensure_devnet_kind_tree().await?;
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
                // The adapter's address is devnet's, so the cluster must be.
                let genesis_hash = rpc
                    .get_genesis_hash()
                    .await
                    .context("failed to fetch the cluster's genesis hash")?;
                ensure_devnet(&genesis_hash)?;
                let env = Self::assemble(None, rpc, Arc::new(payer), pa, prover).await?;
                env.ensure_devnet_kind_tree().await?;
                Ok(env)
            }
        }
    }

    /// Makes solana-devnet's kind tree the prover's, and checks that the
    /// adapter stores its root: the tests prove against devnet's kind tree.
    async fn ensure_devnet_kind_tree(&self) -> anyhow::Result<()> {
        let loaded = super::kind_tree::load_devnet()?;
        let stored = self.protocol_adapter.state().await?.kind_tree_root;
        anyhow::ensure!(
            stored == loaded,
            "the protocol adapter stores the kind tree root {}, the tests prove against {}",
            anoma_rm_risc0::Digest::from_bytes(stored),
            anoma_rm_risc0::Digest::from_bytes(loaded)
        );
        Ok(())
    }
}

/// Refuses a cluster whose genesis hash is `genesis_hash` unless its CAIP-2
/// chain id is devnet's.
fn ensure_devnet(genesis_hash: &solana_message::Hash) -> anyhow::Result<()> {
    let found = chain_id_of_genesis_hash(genesis_hash);
    let devnet = chain_id(ClusterType::Devnet).context("devnet has no recorded genesis hash")?;
    anyhow::ensure!(
        found == devnet,
        "E2E_RPC_URL serves the cluster {found}, not devnet {devnet}, whose adapter the cluster \
         runtime settles on"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cluster_runtime_runs_on_devnet_only() {
        let hash = |cluster: ClusterType| cluster.get_genesis_hash().unwrap();
        ensure_devnet(&hash(ClusterType::Devnet)).expect("devnet is devnet");
        for cluster in [ClusterType::MainnetBeta, ClusterType::Testnet] {
            let refused = ensure_devnet(&hash(cluster)).unwrap_err();
            println!("{cluster:?}: {refused:#}");
            let found = chain_id(cluster).unwrap();
            assert!(
                refused
                    .to_string()
                    .contains(&format!("serves the cluster {found}, not devnet")),
                "{cluster:?}: {refused:#}"
            );
        }
    }
}
