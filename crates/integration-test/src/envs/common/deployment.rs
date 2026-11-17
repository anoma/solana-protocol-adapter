//! The adapter as an environment deploys it on a runtime of its own: devnet's
//! verifier router with its state and the Program Metadata program, from the
//! copies the adapter repository commits (`solana-pa-prototype/devnet-programs`,
//! `dev.sh refresh-devnet-programs`), a verifier registered under the router,
//! and the adapter, initialized against that verifier.

use anchor_lang::AccountDeserialize;
use anoma_pa_solana_client::generated::verifier_router::accounts::VerifierEntry;
use anoma_pa_solana_client::submission::read_verifier_program;
use anoma_pa_solana_client::{AdapterVerifier, derive_verifier_router_pdas};
use anyhow::Context;
use solana_keypair::Keypair;
use solana_signer::Signer;
use surfpool_sdk::{Pubkey, Surfnet};

use super::addresses::{LOCALNET, program_id};
use super::runtime;

/// The deterministic builds of the adapter, at the local address
/// (`dev.sh harness-programs`): for the mock verifier's selector, and for
/// devnet's Groth16 verifier's.
#[cfg(feature = "local")]
const PROTOCOL_ADAPTER_MOCK_SO: &[u8] =
    include_bytes!("../../../programs/protocol_adapter_mock.so");
#[cfg(feature = "e2e")]
const PROTOCOL_ADAPTER_SO: &[u8] = include_bytes!("../../../programs/protocol_adapter.so");

/// Devnet's verifier router, with its router state.
const VERIFIER_ROUTER: &str = "BetEAE4npinksQBxvqUN1KkCVjYFJywWao45MSWtp5yg";
const VERIFIER_ROUTER_SO: &[u8] = include_bytes!(
    "../../../../../solana-pa-prototype/devnet-programs/BetEAE4npinksQBxvqUN1KkCVjYFJywWao45MSWtp5yg.so"
);
const VERIFIER_ROUTER_STATE: &str = include_str!(
    "../../../../../solana-pa-prototype/devnet-programs/9ZJmYSYaYq38GfwQMsEw5gkzfr94Vbzw6Nv53yQuCv2S.json"
);

/// The Program Metadata program, which holds programs' published IDLs.
const PROGRAM_METADATA: &str = "ProgM6JCCvbYkfKqJYHePx4xxSUSqJp7rh8Lyv7nk7S";
const PROGRAM_METADATA_SO: &[u8] = include_bytes!(
    "../../../../../solana-pa-prototype/devnet-programs/ProgM6JCCvbYkfKqJYHePx4xxSUSqJp7rh8Lyv7nk7S.so"
);

/// The harness's mock verifier, and the router's entry registering it under
/// `MOCK_SELECTOR`, as the adapter's local suite loads it.
#[cfg(feature = "local")]
const MOCK_VERIFIER_SO: &[u8] = include_bytes!("../../../programs/mock_verifier.so");
#[cfg(feature = "local")]
const MOCK_VERIFIER_ENTRY: &str = include_str!(
    "../../../../../solana-pa-prototype/tests/fixtures/verifier-entries/verifier-entry-DrnEGMFV4R3mFvf39jtp42rLXtezj2rh5dbFwFnyB632.json"
);

/// Devnet's risc0 3.1.0 Groth16 verifier, and the router's entry registering
/// it under the selector of the seals risc0's Groth16 prover makes.
#[cfg(feature = "e2e")]
const GROTH16_VERIFIER: &str = "5RrWAZFxm9biQ1PxD6azhevG2popHEC3gDJnZ4YoK1Ba";
#[cfg(feature = "e2e")]
const GROTH16_VERIFIER_SO: &[u8] = include_bytes!(
    "../../../../../solana-pa-prototype/devnet-programs/5RrWAZFxm9biQ1PxD6azhevG2popHEC3gDJnZ4YoK1Ba.so"
);
#[cfg(feature = "e2e")]
const GROTH16_VERIFIER_ENTRY: &str = include_str!(
    "../../../../../solana-pa-prototype/devnet-programs/EJEySCEHPgGS1AEz54e2b3SqSzcDJtMuZAbvnSiPJRoS.json"
);

/// The verifier a deployment's adapter accepts the seals of.
pub(in crate::envs) enum Verifier {
    /// The mock verifier, which accepts mock seals.
    #[cfg(feature = "local")]
    Mock,
    /// Devnet's Groth16 verifier, which accepts real proofs.
    #[cfg(feature = "e2e")]
    Groth16,
}

/// Deploys the router, its state and the Program Metadata program, registers
/// `verifier` under the router, and deploys the adapter at its local address
/// and initializes it against `verifier`, owned by `payer`. Returns the
/// adapter's address.
pub(in crate::envs) async fn deploy(
    surfnet: &Surfnet,
    payer: &Keypair,
    verifier: Verifier,
) -> anyhow::Result<Pubkey> {
    let (pa, verifier) = deploy_uninitialized(surfnet, payer, verifier).await?;
    let rpc = runtime::client(surfnet.rpc_url().to_string());
    runtime::initialize(&rpc, payer, pa, &verifier).await?;
    Ok(pa)
}

/// `deploy` up to the adapter's `initialize`: returns the adapter's address
/// and the verifier its build names.
async fn deploy_uninitialized(
    surfnet: &Surfnet,
    payer: &Keypair,
    verifier: Verifier,
) -> anyhow::Result<(Pubkey, AdapterVerifier)> {
    let rpc = runtime::client(surfnet.rpc_url().to_string());
    let router: Pubkey = VERIFIER_ROUTER.parse().expect("a base58 address");
    runtime::deploy(surfnet, router, VERIFIER_ROUTER_SO, payer.pubkey())?;
    let program_metadata: Pubkey = PROGRAM_METADATA.parse().expect("a base58 address");
    runtime::deploy(
        surfnet,
        program_metadata,
        PROGRAM_METADATA_SO,
        payer.pubkey(),
    )?;
    let router_state = runtime::set_account_dump(surfnet, VERIFIER_ROUTER_STATE)?;

    let (verifier_program, entry_dump, verifier_so, adapter_so) = match verifier {
        #[cfg(feature = "local")]
        Verifier::Mock => (
            program_id(LOCALNET, "MOCK_VERIFIER")?,
            MOCK_VERIFIER_ENTRY,
            MOCK_VERIFIER_SO,
            PROTOCOL_ADAPTER_MOCK_SO,
        ),
        #[cfg(feature = "e2e")]
        Verifier::Groth16 => (
            GROTH16_VERIFIER.parse().expect("a base58 address"),
            GROTH16_VERIFIER_ENTRY,
            GROTH16_VERIFIER_SO,
            PROTOCOL_ADAPTER_SO,
        ),
    };
    let entry = runtime::set_account_dump(surfnet, entry_dump)?;
    let entry_data = rpc
        .get_account_data(&entry)
        .await
        .with_context(|| format!("failed to read the verifier entry {entry}"))?;
    let selector = VerifierEntry::try_deserialize(&mut &*entry_data)
        .with_context(|| format!("{entry} is not a verifier entry"))?
        .selector;
    anyhow::ensure!(
        derive_verifier_router_pdas(&router, selector) == (router_state, entry),
        "the committed router state {router_state} and entry {entry} are not the router's \
         accounts for selector {selector:02x?}"
    );
    runtime::deploy(surfnet, verifier_program, verifier_so, payer.pubkey())?;
    let verifier = AdapterVerifier { router, selector };
    let named = read_verifier_program(&rpc, &verifier).await?;
    anyhow::ensure!(
        named == verifier_program,
        "the verifier entry for selector {selector:02x?} names {named}, the deployed verifier \
         is {verifier_program}"
    );

    let pa = program_id(LOCALNET, "PROTOCOL_ADAPTER")?;
    runtime::deploy(surfnet, pa, adapter_so, payer.pubkey())?;
    Ok((pa, verifier))
}

#[cfg(all(test, feature = "local"))]
mod tests {
    use super::*;
    use anchor_lang::AccountSerialize;
    use anoma_pa_solana_client::generated::protocol_adapter::error::ProtocolAdapterError;
    use anoma_pa_solana_client::submission::{Failure, settlement_failure};
    use surfpool_sdk::cheatcodes::builders::SetAccount;

    use super::super::protocol_adapter::preflight_logs;

    /// pa-evm's `test_initialize_reverts_if_the_verifier_is_paused`: once the
    /// router has emergency-stopped the verifier it routes the build's
    /// selector to, `initialize` refuses with `RiscZeroVerifierPaused`.
    #[tokio::test(flavor = "multi_thread")]
    async fn initialize_refuses_a_verifier_the_router_has_paused() -> anyhow::Result<()> {
        let payer = Keypair::new();
        let surfnet = runtime::start(&payer, |builder| builder).await?;
        let rpc = runtime::client(surfnet.rpc_url().to_string());
        let (pa, verifier) = deploy_uninitialized(&surfnet, &payer, Verifier::Mock).await?;

        let (_, entry) = derive_verifier_router_pdas(&verifier.router, verifier.selector);
        let account = rpc.get_account(&entry).await?;
        let mut estopped = VerifierEntry::try_deserialize(&mut &*account.data)?;
        estopped.estopped = true;
        let mut data = Vec::new();
        estopped.try_serialize(&mut data)?;
        surfnet.cheatcodes().execute(
            SetAccount::new(entry)
                .lamports(account.lamports)
                .data(data)
                .owner(account.owner),
        )?;

        let error = runtime::initialize(&rpc, &payer, pa, &verifier)
            .await
            .expect_err("initialize must refuse the paused verifier");
        let logs = preflight_logs(&error)
            .unwrap_or_else(|| panic!("initialize failed without a simulation log: {error:#}"));
        println!("initialize's log:\n{}", logs.join("\n"));
        let failure = settlement_failure(logs, &pa, &Pubkey::default())?;
        assert!(
            matches!(
                failure,
                Some(Failure::Adapter(
                    ProtocolAdapterError::RiscZeroVerifierPaused
                ))
            ),
            "initialize failed with {failure:?}, not RiscZeroVerifierPaused"
        );
        Ok(())
    }
}
