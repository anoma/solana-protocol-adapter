//! pa-testkit's chain-agnostic suite against each environment, and the
//! checks only the Solana adapter's harness makes. Every settlement also
//! checks that the tree pa-testkit builds gives the root the adapter stores.

use anoma_pa_solana_integration_test::envs::local::Environment as SolanaLocalEnv;
use anoma_pa_testkit::assert::{Needle, expect_integration_panic};
use anoma_pa_testkit::fixtures::trivial;
use anoma_pa_testkit::transaction::Transaction;
use anoma_pa_testkit::{execute_tx, prove_actions};
use anoma_rm_risc0::AggregationInstance;
use anyhow::Context;
use solana_signer::Signer;
use surfpool_sdk::Pubkey;

mod local {
    use super::*;

    anoma_pa_testkit::suite_tests!(
        SolanaLocalEnv::setup_bare(),
        // The mock verifier refuses a seal that is not the claim's
        // (programs/mock-verifier).
        refusal = Needle::Static(
            "Error Code: ClaimDigestMismatch. Error Number: 6600. Error Message: mock seal claim \
             digest mismatch."
        ),
        output_mismatch = Needle::Static("Error Code: ExternalCallOutputMismatch."),
    );
}

#[cfg(feature = "e2e")]
mod e2e_test {
    use anoma_pa_solana_integration_test::envs::e2e::Environment as SolanaE2eEnv;

    anoma_pa_testkit::suite_tests!(SolanaE2eEnv::setup_bare());
}

/// The aggregation instance of `tx`, the statement the adapter settles, to
/// tamper with.
fn aggregation_instance(tx: &mut Transaction) -> anyhow::Result<&mut AggregationInstance> {
    Ok(&mut tx
        .as_arm_mut()
        .aggregation
        .as_mut()
        .context("the transaction is aggregated")?
        .instance)
}

/// Checks that the harness refuses to settle `tx` with the error `needle`
/// finds before it sends anything: the payer pays nothing.
async fn refused_before_sending(
    env: &mut SolanaLocalEnv,
    tx: Transaction,
    needle: Needle,
) -> anyhow::Result<()> {
    let payer = env.protocol_adapter.payer.pubkey();
    let rpc = env.protocol_adapter.rpc.clone();
    let before = rpc.get_balance(&payer).await?;
    expect_integration_panic(needle)(execute_tx(env, tx).await)?;
    let after = rpc.get_balance(&payer).await?;
    anyhow::ensure!(
        before == after,
        "the payer paid {} lamports for a transaction that must not be sent",
        before - after
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_root_the_adapter_does_not_store_fails_before_anything_is_sent() -> anyhow::Result<()> {
    let mut env = SolanaLocalEnv::setup_bare().await?;
    let actions = trivial::build_many(2, 71).context("failed to build trivial actions")?;
    let mut tx = prove_actions(&env, &actions).await?;
    aggregation_instance(&mut tx)?.actions[1].consumed_publics[0].commitment_tree_root =
        anoma_rm_risc0::Digest::from_bytes([7; 32]);

    refused_before_sending(
        &mut env,
        tx,
        Needle::Static(
            "consumed commitment tree root not found in PA for action 1 consumed resource 0",
        ),
    )
    .await
}

#[tokio::test(flavor = "multi_thread")]
async fn a_call_to_a_program_no_forwarder_is_registered_for_fails_before_anything_is_sent()
-> anyhow::Result<()> {
    use anoma_pa_solana_client::external_call::{OutputMode, SolanaExternalCall};
    use anoma_rm_risc0::logic_instance::ExpirableBlob;

    let mut env = SolanaLocalEnv::setup_bare().await?;
    let actions = trivial::build_many(1, 81).context("failed to build trivial actions")?;
    let mut tx = prove_actions(&env, &actions).await?;
    let program = Pubkey::new_unique();
    let call = SolanaExternalCall {
        program_id: program.to_bytes(),
        instruction_data: vec![0],
        expected_output: vec![1],
        output_mode: OutputMode::ReturnData,
        num_accounts: 1,
    };
    aggregation_instance(&mut tx)?.actions[0].created_publics[0]
        .app_data
        .external_payload = vec![ExpirableBlob {
        blob: anoma_rm_risc0::utils::bytes_to_words(&call.encode()),
        deletion_criterion: 0,
    }];

    refused_before_sending(
        &mut env,
        tx,
        Needle::Regexp(regex::Regex::new(&regex::escape(&format!(
            "external call 0 is to {program}, for which no forwarder is registered"
        )))?),
    )
    .await
}

#[tokio::test(flavor = "multi_thread")]
async fn the_lookup_table_serves_the_keys_a_forwarder_adds() -> anyhow::Result<()> {
    use solana_address_lookup_table_interface::state::AddressLookupTable;

    let mut env = SolanaLocalEnv::setup_bare().await?;
    let keys: Vec<_> = (0..3).map(|_| Pubkey::new_unique()).collect();
    env.protocol_adapter
        .extend_lookup_table(keys.clone())
        .await?;

    let table = env.protocol_adapter.lookup_table.key;
    let data = env.protocol_adapter.rpc.get_account_data(&table).await?;
    let stored = AddressLookupTable::deserialize(&data)?.addresses.to_vec();
    anyhow::ensure!(
        stored == env.protocol_adapter.lookup_table.addresses,
        "the table stores {stored:?}, the harness compiles against {:?}",
        env.protocol_adapter.lookup_table.addresses
    );
    anyhow::ensure!(
        stored.ends_with(&keys),
        "the table does not end with the added keys"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_consumer_deploys_its_program_and_sends_its_setup() -> anyhow::Result<()> {
    use solana_keypair::Keypair;
    use solana_loader_v3_interface::state::UpgradeableLoaderState;

    let env = SolanaLocalEnv::setup_bare().await?;
    let rpc = env.protocol_adapter.rpc.clone();
    let payer = env.protocol_adapter.payer.pubkey();

    let program = Pubkey::new_unique();
    env.deploy_program(
        program,
        include_bytes!("../programs/mock_verifier.so"),
        payer,
    )?;
    anyhow::ensure!(
        rpc.get_account(&program).await?.executable,
        "the deployed program is not executable"
    );
    let program_data = anoma_pa_solana_client::derive_program_data_address(&program);
    let data = rpc.get_account_data(&program_data).await?;
    let metadata = UpgradeableLoaderState::size_of_programdata_metadata();
    let state: UpgradeableLoaderState = bincode::deserialize(&data[..metadata])?;
    anyhow::ensure!(
        matches!(
            state,
            UpgradeableLoaderState::ProgramData {
                upgrade_authority_address: Some(authority),
                ..
            } if authority == payer
        ),
        "the program's state is {state:?}, not program data upgradeable by {payer}"
    );

    let account = Keypair::new();
    env.send(
        &[solana_system_interface::instruction::create_account(
            &payer,
            &account.pubkey(),
            1_000_000,
            0,
            &solana_system_interface::program::ID,
        )],
        &[&account],
    )
    .await?;
    anyhow::ensure!(
        rpc.get_balance(&account.pubkey()).await? == 1_000_000,
        "the account the extra signer created holds the wrong balance"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_settlements_events_read_back_in_the_order_the_adapter_emitted_them() -> anyhow::Result<()>
{
    use anoma_pa_solana_client::events::PaEvent;
    use anoma_pa_solana_client::settlement_input::settled_resources;
    use anoma_pa_solana_integration_test::executed::Executed;

    let mut env = SolanaLocalEnv::setup_bare().await?;
    let actions = trivial::build_many(2, 91).context("failed to build trivial actions")?;
    let tx = prove_actions(&env, &actions).await?;
    let nullifiers = settled_resources(tx.as_arm())?.nullifiers;

    let signature = env.protocol_adapter.settle(tx).await?;
    let executed = Executed::read(&env.protocol_adapter.rpc, &signature).await?;
    let events = executed.adapter_events(&env.protocol_adapter.program)?;
    let settled: Vec<[u8; 32]> = events
        .iter()
        .filter_map(|event| match event {
            PaEvent::ActionExecuted(action) => Some(action.nullifiers.clone()),
            _ => None,
        })
        .flatten()
        .collect();
    anyhow::ensure!(
        settled == nullifiers,
        "the ActionExecuted events name the nullifiers {settled:02x?}, the transaction consumes \
         {nullifiers:02x?}"
    );
    anyhow::ensure!(
        matches!(events.last(), Some(PaEvent::TransactionExecuted(_))),
        "the last event is {:?}, not TransactionExecuted",
        events.last()
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_settlement_reads_back_with_the_keys_its_lookup_table_loaded() -> anyhow::Result<()> {
    use anoma_pa_solana_integration_test::executed::Executed;

    let mut env = SolanaLocalEnv::setup_bare().await?;
    let actions = trivial::build_many(1, 93).context("failed to build trivial actions")?;
    let tx = prove_actions(&env, &actions).await?;
    let signature = env.protocol_adapter.settle(tx).await?;
    let executed = Executed::read(&env.protocol_adapter.rpc, &signature).await?;

    let (pa_state, _) = anoma_pa_solana_client::derive_pa_state_pda(&env.protocol_adapter.program);
    anyhow::ensure!(
        executed.transaction.signatures == [signature],
        "the transaction read back is signed {:?}, not {signature}",
        executed.transaction.signatures
    );
    anyhow::ensure!(
        executed.loaded.contains(&pa_state)
            && !executed
                .transaction
                .message
                .static_account_keys()
                .contains(&pa_state),
        "the adapter state {pa_state} is not loaded from the settlement lookup table: loaded \
         {:?}",
        executed.loaded
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_consumer_writes_the_loader_buffer_an_upgrade_installs() -> anyhow::Result<()> {
    use solana_keypair::Keypair;
    use solana_loader_v3_interface::state::UpgradeableLoaderState;

    let env = SolanaLocalEnv::setup_bare().await?;
    let so = include_bytes!("../programs/mock_verifier.so");
    let authority = Keypair::new();
    let buffer = env.write_buffer(so, authority.pubkey()).await?;

    let account = env.protocol_adapter.rpc.get_account(&buffer).await?;
    anyhow::ensure!(
        account.owner == solana_sdk_ids::bpf_loader_upgradeable::id(),
        "the buffer is owned by {}, not the upgradeable loader",
        account.owner
    );
    let metadata = UpgradeableLoaderState::size_of_buffer_metadata();
    let state: UpgradeableLoaderState = bincode::deserialize(&account.data[..metadata])?;
    anyhow::ensure!(
        state
            == UpgradeableLoaderState::Buffer {
                authority_address: Some(authority.pubkey()),
            },
        "the buffer's state is {state:?}, not a buffer of {}",
        authority.pubkey()
    );
    anyhow::ensure!(
        account.data[metadata..] == so[..],
        "the buffer does not hold the program written to it"
    );
    Ok(())
}

// A consumer's setup may name the payer among its signers (a mint whose
// authority is the payer); the payer signs once.
#[tokio::test(flavor = "multi_thread")]
async fn a_consumer_names_the_payer_among_its_signers() -> anyhow::Result<()> {
    let env = SolanaLocalEnv::setup_bare().await?;
    let payer = env.protocol_adapter.payer.clone();
    let to = Pubkey::new_unique();
    env.send(
        &[solana_system_interface::instruction::transfer(
            &payer.pubkey(),
            &to,
            1_000_000,
        )],
        &[&payer],
    )
    .await?;
    anyhow::ensure!(
        env.protocol_adapter.rpc.get_balance(&to).await? == 1_000_000,
        "the transfer the payer signed did not land"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn the_local_environment_runs_the_program_metadata_program() -> anyhow::Result<()> {
    let env = SolanaLocalEnv::setup_bare().await?;
    let program: surfpool_sdk::Pubkey = "ProgM6JCCvbYkfKqJYHePx4xxSUSqJp7rh8Lyv7nk7S".parse()?;
    let account = env.protocol_adapter.rpc.get_account(&program).await?;
    anyhow::ensure!(
        account.executable && account.owner == solana_sdk_ids::bpf_loader_upgradeable::id(),
        "{program} is not an upgradeable program: {account:?}"
    );
    Ok(())
}
