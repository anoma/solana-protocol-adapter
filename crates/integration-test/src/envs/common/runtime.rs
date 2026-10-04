//! The surfpool runtime an environment runs on, and the transactions it sends.

use std::sync::Arc;
use std::time::Duration;

use anoma_pa_solana_client::{
    adapter_settlement_lookup_keys, decode_verifier_entry, derive_verifier_entry_pda, initialize_ix,
};
use anyhow::Context;
use base64::Engine;
use serde::Deserialize;
use solana_address_lookup_table_interface::instruction::create_lookup_table;
use solana_address_lookup_table_interface::state::AddressLookupTable;
use solana_commitment_config::CommitmentConfig;
use solana_hash::Hash;
use solana_instruction::Instruction;
use solana_keypair::Keypair;
use solana_loader_v3_interface::state::UpgradeableLoaderState;
use solana_message::{AddressLookupTableAccount, VersionedMessage, v0};
use solana_packet::PACKET_DATA_SIZE;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_signature::Signature;
use solana_signer::Signer;
use solana_transaction::versioned::VersionedTransaction;
use surfpool_sdk::cheatcodes::builders::{CheatcodeBuilder, DeployProgram, SetAccount};
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
/// signed by `payer`, and waits for it to be confirmed; returns its
/// signature. A refused transaction fails with the runtime's simulation logs.
pub(in crate::envs) async fn send(
    rpc: &RpcClient,
    payer: &Keypair,
    instructions: &[Instruction],
    tables: &[AddressLookupTableAccount],
) -> anyhow::Result<Signature> {
    send_signed(rpc, payer, &[], instructions, tables).await
}

/// `send`, with `signers` signing besides `payer`.
pub(in crate::envs) async fn send_signed(
    rpc: &RpcClient,
    payer: &Keypair,
    signers: &[&Keypair],
    instructions: &[Instruction],
    tables: &[AddressLookupTableAccount],
) -> anyhow::Result<Signature> {
    let blockhash = rpc
        .get_latest_blockhash()
        .await
        .context("failed to fetch a blockhash")?;
    let message = v0::Message::try_compile(&payer.pubkey(), instructions, tables, blockhash)
        .context("failed to compile the transaction")?;
    // A signer that is also the payer signs once.
    let all_signers: Vec<&Keypair> = std::iter::once(payer)
        .chain(
            signers
                .iter()
                .copied()
                .filter(|signer| signer.pubkey() != payer.pubkey()),
        )
        .collect();
    let transaction = VersionedTransaction::try_new(VersionedMessage::V0(message), &all_signers)
        .context("failed to sign the transaction")?;
    rpc.send_and_confirm_transaction(&transaction)
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))
}

/// Writes `so` to a new loader buffer whose authority is `authority`, `payer`
/// paying for it, and returns the buffer: the code an upgrade installs. Each
/// write is as large as one packet allows and names its offset, so the
/// writes go out at once.
pub(in crate::envs) async fn write_buffer(
    rpc: &RpcClient,
    payer: &Keypair,
    authority: &Keypair,
    so: &[u8],
) -> anyhow::Result<Pubkey> {
    let buffer = Keypair::new();
    let lamports = rpc
        .get_minimum_balance_for_rent_exemption(UpgradeableLoaderState::size_of_buffer(so.len()))
        .await
        .context("failed to read the buffer's rent")?;
    let create = solana_loader_v3_interface::instruction::create_buffer(
        &payer.pubkey(),
        &buffer.pubkey(),
        &authority.pubkey(),
        lamports,
        so.len(),
    )
    .context("failed to build the buffer's creation")?;
    send_signed(rpc, payer, &[&buffer], &create, &[])
        .await
        .context("failed to create the loader buffer")?;

    // A write's chunk takes what a packet leaves after the same write with a
    // probe chunk. The message encodes the instruction data's length in one
    // byte below 128 and in two from there on, as for every full chunk; the
    // probe chunk's data (16 bytes of header and the chunk) is past 128.
    let write = |offset: usize, chunk: &[u8]| {
        solana_loader_v3_interface::instruction::write(
            &buffer.pubkey(),
            &authority.pubkey(),
            offset as u32,
            chunk.to_vec(),
        )
    };
    const PROBE: usize = 128;
    let probe = v0::Message::try_compile(
        &payer.pubkey(),
        &[write(0, &[0; PROBE])],
        &[],
        Hash::default(),
    )
    .context("failed to compile a buffer write")?;
    let probe = VersionedTransaction {
        signatures: vec![Signature::default(); usize::from(probe.header.num_required_signatures)],
        message: VersionedMessage::V0(probe),
    };
    let chunk_len = PACKET_DATA_SIZE + PROBE
        - bincode::serialize(&probe)
            .context("failed to serialize a buffer write")?
            .len();
    futures::future::try_join_all(so.chunks(chunk_len).enumerate().map(|(i, chunk)| {
        let ix = write(i * chunk_len, chunk);
        async move {
            send_signed(rpc, payer, &[authority], &[ix], &[])
                .await
                .with_context(|| format!("failed to write chunk {i} of the buffer"))
        }
    }))
    .await?;
    Ok(buffer.pubkey())
}

/// surfpool's `surfnet_setProgramAuthority`: the upgrade authority of an
/// upgradeable program.
struct SetProgramAuthority {
    program: Pubkey,
    authority: Pubkey,
}

impl CheatcodeBuilder for SetProgramAuthority {
    const METHOD: &'static str = "surfnet_setProgramAuthority";

    fn build(self) -> serde_json::Value {
        serde_json::json!([self.program.to_string(), self.authority.to_string()])
    }
}

/// Deploys `so` at `program`, upgradeable, with `authority` as its upgrade
/// authority.
pub(in crate::envs) fn deploy(
    surfnet: &Surfnet,
    program: Pubkey,
    so: &[u8],
    authority: Pubkey,
) -> anyhow::Result<()> {
    let cheats = surfnet.cheatcodes();
    cheats
        .deploy(DeployProgram::new(program).so_bytes(so.to_vec()))
        .with_context(|| format!("failed to deploy {program}"))?;
    cheats
        .execute(SetProgramAuthority { program, authority })
        .with_context(|| format!("failed to set the upgrade authority of {program}"))?;
    Ok(())
}

/// An account dump in `solana account --output json` form, as the adapter
/// repository commits its devnet copies and genesis fixtures.
#[derive(Deserialize)]
struct AccountDump {
    pubkey: String,
    account: DumpedAccount,
}

#[derive(Deserialize)]
struct DumpedAccount {
    lamports: u64,
    /// The data and its encoding, which is base64.
    data: (String, String),
    owner: String,
    executable: bool,
}

/// Writes the account `dump` describes, and returns its address.
pub(in crate::envs) fn set_account_dump(surfnet: &Surfnet, dump: &str) -> anyhow::Result<Pubkey> {
    let AccountDump { pubkey, account } =
        serde_json::from_str(dump).context("the account dump is not one")?;
    let address: Pubkey = pubkey.parse().context("the dump's pubkey is not base58")?;
    anyhow::ensure!(
        account.data.1 == "base64",
        "the dump of {address} encodes its data as {}, not base64",
        account.data.1
    );
    let data = base64::engine::general_purpose::STANDARD
        .decode(&account.data.0)
        .context("the dump's data is not base64")?;
    surfnet
        .cheatcodes()
        .execute(
            SetAccount::new(address)
                .lamports(account.lamports)
                .data(data)
                .owner(
                    account
                        .owner
                        .parse()
                        .context("the dump's owner is not base58")?,
                )
                .executable(account.executable),
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
    .context("failed to initialize the protocol adapter")?;
    Ok(())
}

/// The verifier program the router's entry for `selector` names.
pub(in crate::envs) async fn verifier_program(
    rpc: &RpcClient,
    router: Pubkey,
    selector: [u8; 4],
) -> anyhow::Result<Pubkey> {
    let entry = derive_verifier_entry_pda(&router, selector);
    let data = rpc
        .get_account_data(&entry)
        .await
        .with_context(|| format!("the router has no verifier entry {entry}"))?;
    let decoded =
        decode_verifier_entry(&data).with_context(|| format!("the verifier entry {entry}"))?;
    Ok(Pubkey::new_from_array(decoded.verifier))
}

/// Creates the deployment's settlement lookup table with the adapter's part of
/// it, and returns it once the runtime serves its keys.
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
    send(rpc, payer, &[create], &[])
        .await
        .context("failed to create the settlement lookup table")?;
    let keys = adapter_settlement_lookup_keys(&pa, &router, selector, &verifier);
    extend_lookup_table(rpc, payer, table, &keys).await?;
    Ok(AddressLookupTableAccount {
        key: table,
        addresses: keys,
    })
}

/// Adds `keys` to the lookup table `table`, whose authority is `payer`, and
/// returns once the runtime serves them: a table's new keys are usable from
/// the slot after the one that added them.
pub(in crate::envs) async fn extend_lookup_table(
    rpc: &RpcClient,
    payer: &Keypair,
    table: Pubkey,
    keys: &[Pubkey],
) -> anyhow::Result<()> {
    let extend = solana_address_lookup_table_interface::instruction::extend_lookup_table(
        table,
        payer.pubkey(),
        Some(payer.pubkey()),
        keys.to_vec(),
    );
    send(rpc, payer, &[extend], &[])
        .await
        .with_context(|| format!("failed to extend the lookup table {table}"))?;
    let extended_in = AddressLookupTable::deserialize(
        &rpc.get_account_data(&table)
            .await
            .with_context(|| format!("the lookup table {table} does not exist"))?,
    )
    .context("failed to decode the lookup table")?
    .meta
    .last_extended_slot;
    while rpc.get_slot().await.context("failed to fetch the slot")? <= extended_in {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Ok(())
}
