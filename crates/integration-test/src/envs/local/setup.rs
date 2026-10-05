use std::sync::Arc;

use anoma_pa_solana_client::MOCK_SELECTOR;
use anoma_pa_solana_client::derive_verifier_router_pdas;
use anoma_pa_testkit::prover::LocalProver;
use anyhow::Context;
use solana_keypair::Keypair;
use solana_signer::Signer;
use surfpool_sdk::Pubkey;

use super::super::common::addresses::{LOCALNET, program_id};
use super::super::common::runtime;
use super::Environment;

/// The programs the harness ships: the deterministic builds at the local
/// addresses (`dev.sh harness-programs`).
const PROTOCOL_ADAPTER_SO: &[u8] = include_bytes!("../../../programs/protocol_adapter.so");
const MOCK_VERIFIER_SO: &[u8] = include_bytes!("../../../programs/mock_verifier.so");
const TEST_FORWARDER_SO: &[u8] = include_bytes!("../../../programs/test_forwarder.so");
const BLOCK_TIME_FORWARDER_SO: &[u8] = include_bytes!("../../../programs/block_time_forwarder.so");

/// The devnet verifier router the adapter repository commits a copy of, with
/// its router state.
const VERIFIER_ROUTER: &str = "BetEAE4npinksQBxvqUN1KkCVjYFJywWao45MSWtp5yg";
const VERIFIER_ROUTER_SO: &[u8] = include_bytes!(
    "../../../../../solana-pa-prototype/devnet-programs/BetEAE4npinksQBxvqUN1KkCVjYFJywWao45MSWtp5yg.so"
);
const VERIFIER_ROUTER_STATE: &str = include_str!(
    "../../../../../solana-pa-prototype/devnet-programs/9ZJmYSYaYq38GfwQMsEw5gkzfr94Vbzw6Nv53yQuCv2S.json"
);

/// The Program Metadata program, which holds programs' published IDLs, from
/// the devnet copy the adapter repository's local validator loads.
const PROGRAM_METADATA: &str = "ProgM6JCCvbYkfKqJYHePx4xxSUSqJp7rh8Lyv7nk7S";
const PROGRAM_METADATA_SO: &[u8] = include_bytes!(
    "../../../../../solana-pa-prototype/devnet-programs/ProgM6JCCvbYkfKqJYHePx4xxSUSqJp7rh8Lyv7nk7S.so"
);

/// The router's verifier entry registering the mock verifier under
/// `MOCK_SELECTOR`, as the adapter's local suite loads it.
const MOCK_VERIFIER_ENTRY: &str = include_str!(
    "../../../../../solana-pa-prototype/tests/fixtures/verifier-entries/verifier-entry-DrnEGMFV4R3mFvf39jtp42rLXtezj2rh5dbFwFnyB632.json"
);

impl Environment {
    /// Deploys the adapter repository's test forwarder (`programs/test-forwarder`)
    /// at its local address, which it returns: a program whose `forward_call`
    /// fails, returns nothing, logs, writes an account it owns, or relays the
    /// call to another program, as its instruction data says.
    pub fn deploy_test_forwarder(&self) -> anyhow::Result<Pubkey> {
        self.deploy_local_program("TEST_FORWARDER", TEST_FORWARDER_SO)
    }

    /// Deploys the adapter repository's example block-time forwarder
    /// (`programs/block-time-forwarder`) at its local address, which it
    /// returns: a program whose `forward_call` returns how the time it is
    /// given compares with the clock's.
    pub fn deploy_block_time_forwarder(&self) -> anyhow::Result<Pubkey> {
        self.deploy_local_program("BLOCK_TIME_FORWARDER", BLOCK_TIME_FORWARDER_SO)
    }

    /// Deploys `so` at the local address `env/localnet.env` names `name`,
    /// upgradeable by the payer, and returns the address.
    fn deploy_local_program(&self, name: &str, so: &[u8]) -> anyhow::Result<Pubkey> {
        let program = program_id(LOCALNET, name)?;
        self.deploy_program(program, so, self.protocol_adapter.payer.pubkey())?;
        Ok(program)
    }

    pub async fn setup_bare() -> anyhow::Result<Self> {
        let payer = Arc::new(Keypair::new());
        let surfnet = runtime::start(&payer, |builder| builder)
            .await
            .context("failed to start the surfpool runtime")?;
        let rpc = runtime::client(&surfnet);

        let router: Pubkey = VERIFIER_ROUTER.parse().expect("a base58 address");
        let (router_state, mock_entry) = derive_verifier_router_pdas(&router, MOCK_SELECTOR);
        let pa = program_id(LOCALNET, "PROTOCOL_ADAPTER")?;
        let mock_verifier = program_id(LOCALNET, "MOCK_VERIFIER")?;

        runtime::deploy(&surfnet, router, VERIFIER_ROUTER_SO, payer.pubkey())?;
        let program_metadata: Pubkey = PROGRAM_METADATA.parse().expect("a base58 address");
        runtime::deploy(
            &surfnet,
            program_metadata,
            PROGRAM_METADATA_SO,
            payer.pubkey(),
        )?;
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

        Self::assemble(surfnet, payer, pa, LocalProver).await
    }
}
