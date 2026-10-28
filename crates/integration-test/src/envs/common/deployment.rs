//! The adapter as an environment deploys it on a runtime of its own: devnet's
//! verifier router with its state and the Program Metadata program, from the
//! copies the adapter repository commits (`solana-pa-prototype/devnet-programs`,
//! `dev.sh refresh-devnet-programs`), a verifier registered under the router,
//! and the adapter, initialized against that verifier.

use anchor_lang::AccountDeserialize;
use anoma_pa_solana_client::derive_verifier_router_pdas;
use anoma_pa_solana_client::generated::verifier_router::accounts::VerifierEntry;
use anoma_pa_solana_client::submission::read_verifier_program;
use anyhow::Context;
use solana_keypair::Keypair;
use solana_signer::Signer;
use surfpool_sdk::{Pubkey, Surfnet};

use super::addresses::{LOCALNET, program_id};
use super::runtime;

/// The deterministic build of the adapter, at the local address
/// (`dev.sh harness-programs`).
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

/// Devnet's Groth16 verifier, and the router's entry registering it under the
/// selector of the seals risc0's Groth16 prover makes.
#[cfg(feature = "e2e")]
const GROTH16_VERIFIER: &str = "2Yfa83Lzbn71ie3J1KQRiNQz1qHnvVm8gkBCpXZQ7ajD";
#[cfg(feature = "e2e")]
const GROTH16_VERIFIER_SO: &[u8] = include_bytes!(
    "../../../../../solana-pa-prototype/devnet-programs/2Yfa83Lzbn71ie3J1KQRiNQz1qHnvVm8gkBCpXZQ7ajD.so"
);
#[cfg(feature = "e2e")]
const GROTH16_VERIFIER_ENTRY: &str = include_str!(
    "../../../../../solana-pa-prototype/devnet-programs/4ktbrXwBXZMoND5qb3J6abS1m8KqwUtCjjDBebJ4vqey.json"
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
    let rpc = runtime::client(surfnet);
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

    let (verifier_program, entry_dump, verifier_so) = match verifier {
        #[cfg(feature = "local")]
        Verifier::Mock => (
            program_id(LOCALNET, "MOCK_VERIFIER")?,
            MOCK_VERIFIER_ENTRY,
            MOCK_VERIFIER_SO,
        ),
        #[cfg(feature = "e2e")]
        Verifier::Groth16 => (
            GROTH16_VERIFIER.parse().expect("a base58 address"),
            GROTH16_VERIFIER_ENTRY,
            GROTH16_VERIFIER_SO,
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
    let named = read_verifier_program(&rpc, &router, selector).await?;
    anyhow::ensure!(
        named == verifier_program,
        "the verifier entry for selector {selector:02x?} names {named}, the deployed verifier \
         is {verifier_program}"
    );

    let pa = program_id(LOCALNET, "PROTOCOL_ADAPTER")?;
    runtime::deploy(surfnet, pa, PROTOCOL_ADAPTER_SO, payer.pubkey())?;
    runtime::initialize(&rpc, payer, pa, router, selector).await?;
    Ok(pa)
}
