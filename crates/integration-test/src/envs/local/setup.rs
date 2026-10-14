use std::sync::Arc;

use anoma_pa_testkit::prover::LocalProver;
use anyhow::Context;
use solana_keypair::Keypair;
use surfpool_sdk::Pubkey;

use super::super::common::deployment::{self, Verifier};
use super::super::common::environment::JOURNAL_ENCODING;
use super::super::common::runtime;
use super::Environment;

/// The adapter repository's test forwarder, the deterministic build at its
/// local address (`dev.sh harness-programs`).
const TEST_FORWARDER_SO: &[u8] = include_bytes!("../../../programs/test_forwarder.so");

impl Environment {
    /// Deploys the adapter repository's test forwarder (`programs/test-forwarder`)
    /// at its local address, which it returns: a program whose `forward_call`
    /// fails, returns nothing, logs, writes an account it owns, or relays the
    /// call to another program, as its instruction data says.
    pub fn deploy_test_forwarder(&self) -> anyhow::Result<Pubkey> {
        self.deploy_local_program("TEST_FORWARDER", TEST_FORWARDER_SO)
    }

    pub async fn setup_bare() -> anyhow::Result<Self> {
        let payer = Arc::new(Keypair::new());
        let surfnet = runtime::start(&payer, |builder| builder)
            .await
            .context("failed to start the surfpool runtime")?;
        let pa = deployment::deploy(&surfnet, &payer, Verifier::Mock).await?;
        Self::assemble(surfnet, payer, pa, LocalProver::new(JOURNAL_ENCODING)).await
    }
}
