use std::sync::Arc;

use anoma_pa_solana_client::derive_verifier_router_pdas;
use anoma_pa_solana_client::settlement_input::MOCK_SELECTOR;
use anoma_pa_testkit::environment::StateBuilder;
use anoma_pa_testkit::prover::LocalProver;
use anyhow::Context;
use solana_keypair::Keypair;
use solana_signer::Signer;
use surfpool_sdk::Pubkey;

use super::super::common::addresses::{LOCALNET, program_id};
use super::super::common::runtime;
use super::Environment;
use crate::state::cluster::Cluster;

/// The programs the harness ships: the deterministic builds at the local
/// addresses (`dev.sh harness-programs`).
const PROTOCOL_ADAPTER_SO: &[u8] = include_bytes!("../../../programs/protocol_adapter.so");
const MOCK_VERIFIER_SO: &[u8] = include_bytes!("../../../programs/mock_verifier.so");
const TEST_FORWARDER_SO: &[u8] = include_bytes!("../../../programs/test_forwarder.so");

/// The devnet verifier router the adapter repository commits a copy of, with
/// its router state.
const VERIFIER_ROUTER: &str = "BetEAE4npinksQBxvqUN1KkCVjYFJywWao45MSWtp5yg";
const VERIFIER_ROUTER_SO: &[u8] = include_bytes!(
    "../../../../../solana-pa-prototype/devnet-programs/BetEAE4npinksQBxvqUN1KkCVjYFJywWao45MSWtp5yg.so"
);
const VERIFIER_ROUTER_STATE: &str = include_str!(
    "../../../../../solana-pa-prototype/devnet-programs/9ZJmYSYaYq38GfwQMsEw5gkzfr94Vbzw6Nv53yQuCv2S.json"
);

/// The router's verifier entry registering the mock verifier under
/// `MOCK_SELECTOR`, as the adapter's local suite loads it.
const MOCK_VERIFIER_ENTRY: &str = include_str!(
    "../../../../../solana-pa-prototype/tests/fixtures/verifier-entries/verifier-entry-DrnEGMFV4R3mFvf39jtp42rLXtezj2rh5dbFwFnyB632.json"
);

impl Environment {
    /// Deploys the adapter repository's test forwarder (`programs/test-forwarder`)
    /// at its local address, which it returns: a program whose `forward_call`
    /// fails, returns nothing, logs, or relays the call to another program,
    /// as its instruction data says.
    pub fn deploy_test_forwarder(&self) -> anyhow::Result<Pubkey> {
        let program = program_id(LOCALNET, "TEST_FORWARDER")?;
        self.deploy_program(
            program,
            TEST_FORWARDER_SO,
            self.protocol_adapter.payer.pubkey(),
        )?;
        Ok(program)
    }

    pub async fn setup_bare() -> anyhow::Result<Self> {
        Self::setup(async |_| anyhow::Ok(())).await
    }

    pub async fn setup<F>(insert_additional: F) -> anyhow::Result<Self>
    where
        F: AsyncFnOnce(&mut StateBuilder) -> anyhow::Result<()>,
    {
        let payer = Arc::new(Keypair::new());
        let surfnet = runtime::builder(&payer)
            .offline(true)
            .start()
            .await
            .context("failed to start the surfpool runtime")?;
        let rpc = runtime::client(&surfnet);

        let router: Pubkey = VERIFIER_ROUTER.parse().expect("a base58 address");
        let (router_state, mock_entry) = derive_verifier_router_pdas(&router, MOCK_SELECTOR);
        let pa = program_id(LOCALNET, "PROTOCOL_ADAPTER")?;
        let mock_verifier = program_id(LOCALNET, "MOCK_VERIFIER")?;

        runtime::deploy(&surfnet, router, VERIFIER_ROUTER_SO, payer.pubkey())?;
        anyhow::ensure!(
            runtime::set_account_dump(&surfnet, VERIFIER_ROUTER_STATE)? == router_state,
            "the committed router state is not the router's state account {router_state}"
        );
        anyhow::ensure!(
            runtime::set_account_dump(&surfnet, MOCK_VERIFIER_ENTRY)? == mock_entry,
            "the committed mock verifier entry is not the router's entry {mock_entry} for \
             selector {MOCK_SELECTOR:02x?}"
        );
        runtime::deploy(&surfnet, mock_verifier, MOCK_VERIFIER_SO, payer.pubkey())?;
        runtime::deploy(&surfnet, pa, PROTOCOL_ADAPTER_SO, payer.pubkey())?;
        let entry_verifier = runtime::verifier_program(&rpc, router, MOCK_SELECTOR).await?;
        anyhow::ensure!(
            entry_verifier == mock_verifier,
            "the mock verifier entry names {entry_verifier}, the mock verifier is {mock_verifier} \
             (env/localnet.env)"
        );
        runtime::initialize(&rpc, &payer, pa, router, MOCK_SELECTOR).await?;

        Self::assemble(
            surfnet,
            Cluster::Localnet,
            payer,
            pa,
            LocalProver,
            insert_additional,
        )
        .await
    }
}
