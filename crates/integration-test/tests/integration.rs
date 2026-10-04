//! pa-testkit's chain-agnostic suite against each environment, and the
//! checks only the Solana adapter's harness makes. Every settlement also
//! checks that the tree pa-testkit builds gives the root the adapter stores.

use anoma_pa_solana_integration_test::envs::local::Environment as SolanaLocalEnv;
use anoma_pa_testkit::assert::{Needle, expect_integration_panic};
use anoma_pa_testkit::fixtures::trivial;
use anoma_pa_testkit::{execute_tx, prove_actions};
use anyhow::Context;
use solana_signer::Signer;

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
    );
}

#[cfg(feature = "e2e")]
mod e2e_test {
    use anoma_pa_solana_integration_test::envs::e2e::Environment as SolanaE2eEnv;

    anoma_pa_testkit::suite_tests!(SolanaE2eEnv::setup_bare());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_root_the_adapter_does_not_store_fails_before_anything_is_sent() -> anyhow::Result<()> {
    let mut env = SolanaLocalEnv::setup_bare().await?;
    let actions = trivial::build_many(2, 71).context("failed to build trivial actions")?;
    let mut tx = prove_actions(&env, &actions).await?;
    tx.as_arm_mut()
        .aggregation
        .as_mut()
        .context("the transaction is aggregated")?
        .instance
        .actions[1]
        .consumed_publics[0]
        .commitment_tree_root = anoma_rm_risc0::Digest::from_bytes([7; 32]);

    let payer = env.protocol_adapter.payer.pubkey();
    let rpc = env.protocol_adapter.rpc.clone();
    let before = rpc.get_balance(&payer).await?;
    expect_integration_panic(Needle::Static(
        "consumed commitment tree root not found in PA for action 1 consumed resource 0",
    ))(execute_tx(&mut env, tx).await)?;
    let after = rpc.get_balance(&payer).await?;
    anyhow::ensure!(
        before == after,
        "the payer paid {} lamports for a transaction that must not be sent",
        before - after
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_call_to_a_program_no_forwarder_is_registered_for_fails_before_anything_is_sent()
-> anyhow::Result<()> {
    use anoma_pa_solana_client::external_call::{OutputMode, SolanaExternalCall};
    use anoma_rm_risc0::logic_instance::ExpirableBlob;

    let mut env = SolanaLocalEnv::setup_bare().await?;
    let actions = trivial::build_many(1, 81).context("failed to build trivial actions")?;
    let mut tx = prove_actions(&env, &actions).await?;
    let program = solana_signer::Signer::pubkey(&solana_keypair::Keypair::new());
    let call = SolanaExternalCall {
        program_id: program.to_bytes(),
        instruction_data: vec![0],
        expected_output: vec![1],
        output_mode: OutputMode::ReturnData,
        num_accounts: 1,
    };
    tx.as_arm_mut()
        .aggregation
        .as_mut()
        .context("the transaction is aggregated")?
        .instance
        .actions[0]
        .created_publics[0]
        .app_data
        .external_payload = vec![ExpirableBlob {
        blob: anoma_rm_risc0::utils::bytes_to_words(&call.encode()),
        deletion_criterion: 0,
    }];

    let payer = env.protocol_adapter.payer.pubkey();
    let rpc = env.protocol_adapter.rpc.clone();
    let before = rpc.get_balance(&payer).await?;
    expect_integration_panic(Needle::Regexp(regex::Regex::new(&regex::escape(
        &format!("external call 0 is to {program}, for which no forwarder is registered"),
    ))?))(execute_tx(&mut env, tx).await)?;
    anyhow::ensure!(
        rpc.get_balance(&payer).await? == before,
        "the payer paid for a transaction that must not be sent"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn the_lookup_table_serves_the_keys_a_forwarder_adds() -> anyhow::Result<()> {
    use solana_address_lookup_table_interface::state::AddressLookupTable;

    let mut env = SolanaLocalEnv::setup_bare().await?;
    let keys: Vec<_> = (0..3)
        .map(|_| solana_signer::Signer::pubkey(&solana_keypair::Keypair::new()))
        .collect();
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

    let env = SolanaLocalEnv::setup_bare().await?;
    let rpc = env.protocol_adapter.rpc.clone();
    let payer = env.protocol_adapter.payer.pubkey();

    let program = Keypair::new().pubkey();
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
    // UpgradeableLoaderState::ProgramData: tag (4), slot (8), Option<Pubkey>.
    anyhow::ensure!(
        data[12] == 1 && data[13..45] == payer.to_bytes(),
        "the program's upgrade authority is not the one given"
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
    use anoma_pa_solana_client::events::{PaEvent, decode_event_instruction};
    use anoma_pa_solana_client::settlement_input::settled_resources;
    use anoma_pa_solana_integration_test::executed::Executed;

    let mut env = SolanaLocalEnv::setup_bare().await?;
    let actions = trivial::build_many(2, 91).context("failed to build trivial actions")?;
    let tx = prove_actions(&env, &actions).await?;
    let nullifiers = settled_resources(tx.as_arm())?.nullifiers;

    let signature = env.protocol_adapter.settle(tx).await?;
    let executed = Executed::read(&env.protocol_adapter.rpc, &signature).await?;
    let events = executed
        .cpi_events(&env.protocol_adapter.program)
        .map(decode_event_instruction)
        .collect::<Result<Vec<_>, _>>()?;
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
async fn the_test_forwarders_log_mode_fills_the_transactions_log() -> anyhow::Result<()> {
    use anoma_pa_solana_integration_test::executed::Executed;
    use anoma_pa_solana_integration_test::test_forwarder;

    let env = SolanaLocalEnv::setup_bare().await?;
    let program = env.deploy_test_forwarder()?;
    // Agave keeps 10,000 bytes of program log per transaction, counting each
    // line with its "Program log: " prefix; 100 of the forwarder's 100-byte
    // lines exceed it.
    let signature = env
        .send(&[test_forwarder::log_ix(&program, 100)], &[])
        .await?;
    let executed = Executed::read(&env.protocol_adapter.rpc, &signature).await?;
    anyhow::ensure!(
        executed.logs.iter().any(|line| line == "Log truncated"),
        "the transaction's log is not truncated: {:?}",
        executed.logs
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
