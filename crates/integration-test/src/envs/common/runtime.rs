//! The surfpool runtime an environment runs on, and the transactions it sends.

use std::sync::Arc;
use std::time::Duration;

use anoma_pa_solana_client::{
    adapter_settlement_lookup_keys, decode_pa_state, decode_verifier_entry, derive_pa_state_pda,
    derive_verifier_entry_pda, encode_pa_state, initialize_ix,
};
use anyhow::Context;
use solana_address_lookup_table_interface::instruction::create_lookup_table;
use solana_address_lookup_table_interface::state::AddressLookupTable;
use solana_commitment_config::CommitmentConfig;
use solana_instruction::Instruction;
use solana_keypair::Keypair;
use solana_loader_v3_interface::state::UpgradeableLoaderState;
use solana_message::{AddressLookupTableAccount, Hash, VersionedMessage, v0};
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_rpc_client_types::response::RpcKeyedAccount;
use solana_signature::Signature;
use solana_signer::Signer;
use solana_transaction::versioned::VersionedTransaction;
use surfpool_sdk::cheatcodes::builders::{CheatcodeBuilder, DeployProgram, SetAccount};
use surfpool_sdk::{
    BlockProductionMode, Pubkey, Surfnet, SurfnetBuilder, SurfnetError, SurfnetResult,
};

/// The SOL the default signer starts with. surfpool refuses an airdrop of a
/// million SOL; this covers every upload and settlement of a test run.
const PAYER_LAMPORTS: u64 = 500_000_000_000;

/// A runtime producing a block every 400 ms, mainnet's slot time, with a fresh
/// funded default signer.
fn builder(payer: &Keypair) -> SurfnetBuilder {
    Surfnet::builder()
        .block_production_mode(BlockProductionMode::Clock)
        .slot_time_ms(400)
        .airdrop_sol(PAYER_LAMPORTS)
        .payer(payer.insecure_clone())
}

/// Held while a runtime starts. surfpool picks each of a runtime's ports by
/// binding port 0 and releasing it, then binds the port again when its
/// servers start, so a runtime starting at the same time can be handed the
/// same port: one of them then fails to bind it, or, when its start-up check
/// finds the port taken, fails without saying so (below).
static STARTING: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// How long a start may take. A start takes about a second, 7.3 s at most
/// observed with 48 queued behind one another; surfpool only logs a failure
/// before the runtime is ready (its runloop's error), and its start then
/// waits for a ready signal that never comes, so a start that takes longer
/// has failed.
const START_DEADLINE: Duration = Duration::from_secs(60);

/// Starts the runtime `configure` makes of the harness's (`builder`), once no
/// other runtime of this process is starting. The two picks of one start can
/// also return the same port, the second server then failing to bind it; such
/// a start binds nothing, so the runtime starts again on newly picked ports.
pub(in crate::envs) async fn start(
    payer: &Keypair,
    configure: impl Fn(SurfnetBuilder) -> SurfnetBuilder,
) -> anyhow::Result<Surfnet> {
    let _starting = STARTING.lock().await;
    let mut attempt = 1;
    loop {
        match start_within(configure(builder(payer)), START_DEADLINE).await? {
            Err(SurfnetError::Aborted(error)) if error.contains("AddrInUse") => {
                eprintln!("surfpool start {attempt} picked a port twice, starting again: {error}");
                attempt += 1;
            }
            started => return Ok(started?),
        }
    }
}

/// Starts `builder`'s runtime on a thread of its own, since surfpool blocks
/// the thread it starts on until the runtime is ready; fails when that takes
/// longer than `deadline`.
async fn start_within(
    builder: SurfnetBuilder,
    deadline: Duration,
) -> anyhow::Result<SurfnetResult<Surfnet>> {
    let (done, started) = tokio::sync::oneshot::channel();
    std::thread::Builder::new()
        .name("surfpool-start".into())
        .spawn(move || {
            let started = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(1)
                .enable_all()
                .build()
                .map(|runtime| runtime.block_on(builder.start()));
            if done.send(started).is_err() {
                // The caller passed its deadline and reported the failure;
                // a runtime that started after all shuts down as it drops.
            }
        })
        .context("failed to spawn the runtime's start-up thread")?;
    tokio::time::timeout(deadline, started)
        .await
        .map_err(|_| {
            anyhow::anyhow!(
                "the surfpool runtime did not start within {deadline:?}: surfpool failed while \
                 starting and only logged why"
            )
        })?
        .context("the runtime's start-up thread ended without a result")?
        .context("failed to build the start-up thread's tokio runtime")
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
/// signed by `payer` and `signers`, and waits for it to be confirmed; returns
/// its signature. A refused transaction fails with the runtime's simulation
/// logs.
pub(in crate::envs) async fn send_signed(
    rpc: &RpcClient,
    payer: &Keypair,
    signers: &[&Keypair],
    instructions: &[Instruction],
    tables: &[AddressLookupTableAccount],
) -> anyhow::Result<Signature> {
    let blockhash = latest_blockhash(rpc).await?;
    send_with_blockhash(rpc, payer, signers, instructions, tables, blockhash).await
}

/// The runtime's latest blockhash.
pub(in crate::envs) async fn latest_blockhash(rpc: &RpcClient) -> anyhow::Result<Hash> {
    rpc.get_latest_blockhash()
        .await
        .context("failed to fetch a blockhash")
}

/// `send_signed`, with the transaction built on `blockhash`.
pub(in crate::envs) async fn send_with_blockhash(
    rpc: &RpcClient,
    payer: &Keypair,
    signers: &[&Keypair],
    instructions: &[Instruction],
    tables: &[AddressLookupTableAccount],
    blockhash: Hash,
) -> anyhow::Result<Signature> {
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
    // The client's error keeps the runtime's simulation, which a refused
    // settlement is decoded from.
    rpc.send_and_confirm_transaction(&transaction)
        .await
        .map_err(anyhow::Error::from)
}

/// Places `so` in a new loader buffer whose authority is `authority`, and
/// returns the buffer: the code an upgrade installs. Like `deploy`, it sets
/// the account the loader's writes would leave, the loader's buffer state
/// followed by the code, at the rent-exempt balance; a program the size of
/// the adapter would take hundreds of write transactions.
pub(in crate::envs) async fn write_buffer(
    surfnet: &Surfnet,
    rpc: &RpcClient,
    authority: Pubkey,
    so: &[u8],
) -> anyhow::Result<Pubkey> {
    let buffer = Keypair::new().pubkey();
    let mut data = bincode::serialize(&UpgradeableLoaderState::Buffer {
        authority_address: Some(authority),
    })
    .context("failed to encode the buffer state")?;
    data.extend_from_slice(so);
    let lamports = rpc
        .get_minimum_balance_for_rent_exemption(data.len())
        .await
        .context("failed to read the buffer's rent")?;
    surfnet
        .cheatcodes()
        .execute(
            SetAccount::new(buffer)
                .lamports(lamports)
                .data(data)
                .owner(solana_sdk_ids::bpf_loader_upgradeable::id()),
        )
        .with_context(|| format!("failed to set the buffer {buffer}"))?;
    Ok(buffer)
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

/// Writes the account `dump` describes, and returns its address: a dump in
/// `solana account --output json` form, as the adapter repository commits
/// its devnet copies and genesis fixtures.
pub(in crate::envs) fn set_account_dump(surfnet: &Surfnet, dump: &str) -> anyhow::Result<Pubkey> {
    let RpcKeyedAccount { pubkey, account } =
        serde_json::from_str(dump).context("the account dump is not one")?;
    let address: Pubkey = pubkey.parse().context("the dump's pubkey is not base58")?;
    let data = account
        .data
        .decode()
        .with_context(|| format!("the dump of {address} holds no binary data"))?;
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

/// Makes `owner` the owner of the adapter `pa` on a runtime forking a
/// deployment: rewrites the adapter's state with it, keeping the rest of the
/// state and the account's allocation.
pub(in crate::envs) async fn take_ownership(
    surfnet: &Surfnet,
    rpc: &RpcClient,
    pa: Pubkey,
    owner: Pubkey,
) -> anyhow::Result<()> {
    let (pa_state, _) = derive_pa_state_pda(&pa);
    let account = rpc
        .get_account(&pa_state)
        .await
        .with_context(|| format!("the protocol adapter {pa} has no state account {pa_state}"))?;
    let mut state = decode_pa_state(&account.data).context("failed to decode the adapter state")?;
    state.owner = owner.to_bytes();
    let encoded = encode_pa_state(&state);
    let mut data = account.data;
    anyhow::ensure!(
        encoded.len() <= data.len(),
        "the adapter state encodes to {} bytes, more than its account's {}",
        encoded.len(),
        data.len()
    );
    data[..encoded.len()].copy_from_slice(&encoded);
    surfnet
        .cheatcodes()
        .execute(
            SetAccount::new(pa_state)
                .lamports(account.lamports)
                .data(data)
                .owner(account.owner),
        )
        .with_context(|| format!("failed to write the adapter state {pa_state}"))?;
    Ok(())
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
    send_signed(
        rpc,
        payer,
        &[],
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
    send_signed(rpc, payer, &[], &[create], &[])
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
    send_signed(rpc, payer, &[], &[extend], &[])
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Many runtimes starting at once in one process all come up: each
    /// start picks its ports while the others pick theirs. 48 at once
    /// failed 14 runs in 40 before starts waited for one another.
    #[tokio::test(flavor = "multi_thread")]
    async fn runtimes_starting_at_once_all_come_up() {
        const STARTS: usize = 48;
        let starts = (0..STARTS).map(|i| async move {
            let started = start(&Keypair::new(), |builder| builder).await;
            started
                .map_err(|error| format!("start {i}: {error:#}"))
                .err()
        });
        let failures: Vec<String> = futures::future::join_all(starts)
            .await
            .into_iter()
            .flatten()
            .collect();
        assert!(failures.is_empty(), "{failures:#?}");
    }

    /// A runtime that fails while starting (here, forking a cluster no RPC
    /// endpoint serves) fails its start: surfpool only logs such a failure,
    /// and its start waits for a ready signal that never comes.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_runtime_that_fails_to_start_fails_its_start() {
        let forking_nothing =
            builder(&Keypair::new()).remote_rpc_url("http://127.0.0.1:9".to_string());
        match start_within(forking_nothing, Duration::from_secs(10)).await {
            Err(error) => assert!(
                error.to_string().contains("did not start within"),
                "the start failed otherwise: {error:#}"
            ),
            Ok(Ok(surfnet)) => panic!(
                "a runtime forking no cluster started at {}",
                surfnet.rpc_url()
            ),
            Ok(Err(error)) => panic!("surfpool reported the failure itself: {error}"),
        }
    }
}
