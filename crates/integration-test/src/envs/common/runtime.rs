//! The surfpool runtime an environment runs on, and the transactions it sends.

use std::sync::Arc;
use std::time::Duration;

use anoma_pa_solana_client::{
    adapter_settlement_lookup_keys, derive_verifier_entry_pda, initialize_ix,
};
use anyhow::Context;
use solana_address_lookup_table_interface::instruction::{
    create_lookup_table, extend_lookup_table,
};
use solana_address_lookup_table_interface::state::AddressLookupTable;
use solana_commitment_config::CommitmentConfig;
use solana_instruction::Instruction;
use solana_keypair::Keypair;
use solana_message::{AddressLookupTableAccount, VersionedMessage, v0};
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_signer::Signer;
use solana_transaction::versioned::VersionedTransaction;
use surfpool_sdk::cheatcodes::builders::{DeployProgram, SetAccount};
use surfpool_sdk::{BlockProductionMode, Pubkey, Surfnet, SurfnetBuilder};

/// The SOL the default signer starts with. surfpool refuses an airdrop of a
/// million SOL; this covers every upload and settlement of a test run.
const PAYER_LAMPORTS: u64 = 500_000_000_000;

/// A runtime producing a block every 400 ms, mainnet's slot time, with a fresh
/// funded default signer.
pub(in crate::envs) fn builder(payer: &Keypair) -> SurfnetBuilder {
    Surfnet::builder()
        .block_production_mode(BlockProductionMode::Clock)
        .slot_time_ms(400)
        .airdrop_sol(PAYER_LAMPORTS)
        .payer(payer.insecure_clone())
}

/// A client of the runtime at `confirmed` commitment: the runtime produces
/// blocks on a clock and never finalizes them.
pub(in crate::envs) fn client(surfnet: &Surfnet) -> Arc<RpcClient> {
    Arc::new(RpcClient::new_with_commitment(
        surfnet.rpc_url().to_string(),
        CommitmentConfig::confirmed(),
    ))
}

/// Sends `instructions` as one v0 transaction compiled against `tables`,
/// signed by `payer`, and waits for it to be confirmed. A refused transaction
/// fails with the runtime's simulation logs.
pub(in crate::envs) async fn send(
    rpc: &RpcClient,
    payer: &Keypair,
    instructions: &[Instruction],
    tables: &[AddressLookupTableAccount],
) -> anyhow::Result<()> {
    let blockhash = rpc
        .get_latest_blockhash()
        .await
        .context("failed to fetch a blockhash")?;
    let message = v0::Message::try_compile(&payer.pubkey(), instructions, tables, blockhash)
        .context("failed to compile the transaction")?;
    let transaction = VersionedTransaction::try_new(VersionedMessage::V0(message), &[payer])
        .context("failed to sign the transaction")?;
    rpc.send_and_confirm_transaction(&transaction)
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    Ok(())
}

/// Deploys `so` at `program`, upgradeable, with `authority` as its upgrade
/// authority.
pub(in crate::envs) async fn deploy(
    surfnet: &Surfnet,
    rpc: &RpcClient,
    program: Pubkey,
    so: &[u8],
    authority: Pubkey,
) -> anyhow::Result<()> {
    surfnet
        .cheatcodes()
        .deploy(DeployProgram::new(program).so_bytes(so.to_vec()))
        .with_context(|| format!("failed to deploy {program}"))?;
    let _: serde_json::Value = rpc
        .send(
            solana_rpc_client_api::request::RpcRequest::Custom {
                method: "surfnet_setProgramAuthority",
            },
            serde_json::json!([program.to_string(), authority.to_string()]),
        )
        .await
        .with_context(|| format!("failed to set the upgrade authority of {program}"))?;
    Ok(())
}

/// Writes an account dump in `solana account --output json` form, as the
/// adapter repository commits its devnet copies and genesis fixtures.
pub(in crate::envs) fn set_account_dump(surfnet: &Surfnet, dump: &str) -> anyhow::Result<Pubkey> {
    use base64::Engine;

    let value: serde_json::Value =
        serde_json::from_str(dump).context("the account dump is not JSON")?;
    let field = |path: &str| {
        value
            .pointer(path)
            .with_context(|| format!("the account dump has no {path}"))
    };
    let address: Pubkey = field("/pubkey")?
        .as_str()
        .context("pubkey is not a string")?
        .parse()
        .context("pubkey is not base58")?;
    let data = base64::engine::general_purpose::STANDARD
        .decode(
            field("/account/data/0")?
                .as_str()
                .context("data is not a string")?,
        )
        .context("data is not base64")?;
    surfnet
        .cheatcodes()
        .execute(
            SetAccount::new(address)
                .lamports(field("/account/lamports")?.as_u64().context("lamports")?)
                .data(data)
                .owner(
                    field("/account/owner")?
                        .as_str()
                        .context("owner is not a string")?
                        .parse()
                        .context("owner is not base58")?,
                )
                .executable(
                    field("/account/executable")?
                        .as_bool()
                        .context("executable is not a bool")?,
                ),
        )
        .with_context(|| format!("failed to set account {address}"))?;
    Ok(address)
}

/// Initializes the adapter `pa`, deployed with `payer` as its upgrade
/// authority: `payer` becomes its owner, with `router` and `selector`.
pub(in crate::envs) async fn initialize(
    rpc: &RpcClient,
    payer: &Keypair,
    pa: Pubkey,
    router: Pubkey,
    selector: [u8; 4],
) -> anyhow::Result<()> {
    send(
        rpc,
        payer,
        &[initialize_ix(
            &pa,
            &payer.pubkey(),
            &payer.pubkey(),
            &router,
            selector,
        )],
        &[],
    )
    .await
    .context("failed to initialize the protocol adapter")
}

/// The verifier program the router's entry for `selector` names. The entry is
/// the router's `VerifierEntry { selector: [u8; 4], verifier: Pubkey, paused:
/// bool }` behind Anchor's 8-byte discriminator.
pub(in crate::envs) async fn verifier_program(
    rpc: &RpcClient,
    router: Pubkey,
    selector: [u8; 4],
) -> anyhow::Result<Pubkey> {
    const VERIFIER: std::ops::Range<usize> = 12..44;
    let entry = derive_verifier_entry_pda(&router, selector);
    let data = rpc
        .get_account_data(&entry)
        .await
        .with_context(|| format!("the router has no verifier entry {entry}"))?;
    let verifier = data
        .get(VERIFIER)
        .with_context(|| format!("the verifier entry {entry} holds {} bytes", data.len()))?;
    Ok(Pubkey::try_from(verifier).expect("a 32-byte slice is an address"))
}

/// Creates the deployment's settlement lookup table with the adapter's part of
/// it, and returns it once the runtime serves its keys: a table's new keys are
/// usable from the slot after the one that added them.
pub(in crate::envs) async fn create_settlement_lookup_table(
    rpc: &RpcClient,
    payer: &Keypair,
    pa: Pubkey,
    router: Pubkey,
    selector: [u8; 4],
    verifier: Pubkey,
) -> anyhow::Result<AddressLookupTableAccount> {
    let recent_slot = rpc.get_slot().await.context("failed to fetch the slot")?;
    let (create, table) = create_lookup_table(payer.pubkey(), payer.pubkey(), recent_slot);
    let keys = adapter_settlement_lookup_keys(&pa, &router, selector, &verifier);
    let extend = extend_lookup_table(table, payer.pubkey(), Some(payer.pubkey()), keys.clone());
    send(rpc, payer, &[create, extend], &[])
        .await
        .context("failed to create the settlement lookup table")?;

    let extended_in = AddressLookupTable::deserialize(
        &rpc.get_account_data(&table)
            .await
            .context("the settlement lookup table does not exist")?,
    )
    .context("failed to decode the settlement lookup table")?
    .meta
    .last_extended_slot;
    while rpc.get_slot().await.context("failed to fetch the slot")? <= extended_in {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Ok(AddressLookupTableAccount {
        key: table,
        addresses: keys,
    })
}
