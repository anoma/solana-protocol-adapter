//! The adapter's external calls, made to the test forwarder from a
//! pass-through action: the accounts a forwarder receives, its output checked
//! against the proof's, the instructions a settlement transaction carries
//! before the settlement, the events the adapter emits, and the settlement's
//! size.

use std::sync::Arc;

use anoma_pa_solana_client::events::{PaEvent, decode_event_instruction};
use anoma_pa_solana_client::external_call::{OutputMode, SolanaExternalCall};
use anoma_pa_solana_integration_test::envs::local::Environment as SolanaLocalEnv;
use anoma_pa_solana_integration_test::executed::Executed;
use anoma_pa_solana_integration_test::forwarders::{CallAccounts, Forwarder};
use anoma_pa_solana_integration_test::test_forwarder::{
    RELAY_OK, log_ix, relay_input, write_input,
};
use anoma_pa_testkit::assert::{Needle, expect_integration_panic};
use anoma_pa_testkit::fixtures::passthrough::{self, PASSTHROUGH_LOGIC_VK};
use anoma_pa_testkit::transaction::Transaction;
use anoma_pa_testkit::witness::{AppData, ExpirableBlob};
use anoma_pa_testkit::{execute_tx, prove_actions};
use anoma_rm_risc0::utils::bytes_to_words;
use futures::future::BoxFuture;
use solana_instruction::{AccountMeta, Instruction};
use solana_keypair::Keypair;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_signer::Signer;
use surfpool_sdk::Pubkey;

/// What the write calls write, and what the account holds before.
const WRITTEN: [u8; 4] = [1, 2, 3, 4];
const UNWRITTEN: [u8; 4] = [0; 4];

/// A forwarder whose every call takes the same accounts.
struct Fixed(CallAccounts);

impl Forwarder for Fixed {
    fn call_accounts<'a>(
        &'a self,
        _rpc: &'a RpcClient,
        _call: &'a SolanaExternalCall,
    ) -> BoxFuture<'a, anyhow::Result<CallAccounts>> {
        Box::pin(async move {
            Ok(CallAccounts {
                segment: self.0.segment.clone(),
                preceding: self.0.preceding.clone(),
            })
        })
    }
}

/// The test forwarder, deployed, and an account it owns for its write mode
/// to write to.
struct Setup {
    env: SolanaLocalEnv,
    forwarder: Pubkey,
    account: Pubkey,
}

async fn setup() -> anyhow::Result<Setup> {
    let env = SolanaLocalEnv::setup_bare().await?;
    let forwarder = env.deploy_test_forwarder()?;
    let account = Keypair::new();
    let payer = env.protocol_adapter.payer.pubkey();
    let len = WRITTEN.len();
    let rent = env
        .protocol_adapter
        .rpc
        .get_minimum_balance_for_rent_exemption(len)
        .await?;
    env.send(
        &[solana_system_interface::instruction::create_account(
            &payer,
            &account.pubkey(),
            rent,
            len as u64,
            &forwarder,
        )],
        &[&account],
    )
    .await?;
    Ok(Setup {
        env,
        forwarder,
        account: account.pubkey(),
    })
}

impl Setup {
    /// Registers the test forwarder's calls to take `segment` after the
    /// forwarder, with `preceding` before the settlement.
    fn register(&mut self, segment: Vec<AccountMeta>, preceding: Vec<Instruction>) {
        let segment = std::iter::once(AccountMeta::new_readonly(self.forwarder, false))
            .chain(segment)
            .collect();
        self.env.protocol_adapter.forwarders.register(
            self.forwarder,
            Arc::new(Fixed(CallAccounts { segment, preceding })),
        );
    }

    /// Proves a pass-through action, with nonces from `seed`, that calls the
    /// test forwarder with `input`, expecting `expected`, and passes it
    /// `extra_accounts` accounts after the forwarder.
    async fn prove_call(
        &self,
        seed: u8,
        input: Vec<u8>,
        expected: &[u8],
        extra_accounts: u8,
    ) -> anyhow::Result<Transaction> {
        let call = SolanaExternalCall {
            program_id: self.forwarder.to_bytes(),
            instruction_data: input,
            expected_output: expected.to_vec(),
            output_mode: OutputMode::ReturnData,
            num_accounts: 1 + extra_accounts,
        };
        let app_data = AppData {
            external_payload: vec![ExpirableBlob {
                blob: bytes_to_words(&call.encode()),
                deletion_criterion: 0,
            }],
            ..AppData::default()
        };
        let action = passthrough::build(seed, app_data, passthrough::Overrides::default())?;
        prove_actions(&self.env, &[action.witnesses]).await
    }

    /// The first bytes of the written account.
    async fn written(&self) -> anyhow::Result<Vec<u8>> {
        let data = self
            .env
            .protocol_adapter
            .rpc
            .get_account_data(&self.account)
            .await?;
        Ok(data[..WRITTEN.len()].to_vec())
    }

    /// The adapter's events of the settlement `executed`, in the order it
    /// emitted them.
    fn events(&self, executed: &Executed) -> anyhow::Result<Vec<PaEvent>> {
        Ok(executed
            .cpi_events(&self.env.protocol_adapter.program)
            .map(decode_event_instruction)
            .collect::<Result<Vec<_>, _>>()?)
    }
}

// The segment's accounts reach the forwarder with the writability the
// submitter gives them, so a forwarder can change the state its call names.
#[tokio::test(flavor = "multi_thread")]
async fn a_forwarder_writes_an_account_its_segment_passes_writable() -> anyhow::Result<()> {
    let mut s = setup().await?;
    s.register(vec![AccountMeta::new(s.account, false)], vec![]);
    let tx = s.prove_call(1, write_input(&WRITTEN), &WRITTEN, 1).await?;
    execute_tx(&mut s.env, tx).await?;
    let written = s.written().await?;
    anyhow::ensure!(
        written == WRITTEN,
        "the account holds {written:?}, not the {WRITTEN:?} the forwarder wrote"
    );
    Ok(())
}

// The adapter compares a forwarder's return data with the output the proof
// expects after the forwarder has changed state, and the refusal undoes the
// change.
#[tokio::test(flavor = "multi_thread")]
async fn refuses_a_call_whose_forwarder_changes_state_and_returns_other_than_expected()
-> anyhow::Result<()> {
    let mut s = setup().await?;
    s.register(vec![AccountMeta::new(s.account, false)], vec![]);
    let tx = s.prove_call(2, write_input(&WRITTEN), &[9; 4], 1).await?;
    expect_integration_panic(Needle::Static("Error Code: ExternalCallOutputMismatch."))(
        execute_tx(&mut s.env, tx).await,
    )?;
    let written = s.written().await?;
    anyhow::ensure!(
        written == UNWRITTEN,
        "the refused settlement left the account holding {written:?}"
    );
    Ok(())
}

// A segment can name more than one program: the test forwarder relays the
// call to the program after it (itself, which writes the account), and that
// program's call takes the accounts after it.
#[tokio::test(flavor = "multi_thread")]
async fn a_forwarder_calls_a_second_program_its_segment_names() -> anyhow::Result<()> {
    let mut s = setup().await?;
    s.register(
        vec![
            AccountMeta::new_readonly(s.forwarder, false),
            AccountMeta::new(s.account, false),
        ],
        vec![],
    );
    let input = relay_input(PASSTHROUGH_LOGIC_VK.into(), &write_input(&WRITTEN));
    let tx = s.prove_call(3, input, &[RELAY_OK], 2).await?;
    let signature = s.env.protocol_adapter.settle(tx).await?;
    let executed = Executed::read(&s.env.protocol_adapter.rpc, &signature).await?;
    // The adapter runs at depth 1, the forwarder at 2, the relayed call at 3.
    let relayed = format!("Program {} invoke [3]", s.forwarder);
    anyhow::ensure!(
        executed.logs.contains(&relayed),
        "the forwarder did not call the second program ({relayed}): {:?}",
        executed.logs
    );
    let written = s.written().await?;
    anyhow::ensure!(
        written == WRITTEN,
        "the account holds {written:?}, not the {WRITTEN:?} the relayed call wrote"
    );
    Ok(())
}

// A submitter puts the instructions a call needs (an ed25519 signature
// check, an account's creation) before the settlement, in its transaction.
#[tokio::test(flavor = "multi_thread")]
async fn a_settlement_settles_after_other_instructions_in_its_transaction() -> anyhow::Result<()> {
    let mut s = setup().await?;
    let preceding = log_ix(&s.forwarder, 1);
    s.register(
        vec![AccountMeta::new(s.account, false)],
        vec![preceding.clone()],
    );
    let tx = s.prove_call(4, write_input(&WRITTEN), &WRITTEN, 1).await?;
    let signature = s.env.protocol_adapter.settle(tx).await?;
    let executed = Executed::read(&s.env.protocol_adapter.rpc, &signature).await?;
    let message = &executed.transaction.message;
    let first = &message.instructions()[0];
    anyhow::ensure!(
        message.static_account_keys()[usize::from(first.program_id_index)] == s.forwarder
            && first.data == preceding.data,
        "the settlement transaction does not start with the preceding instruction"
    );
    anyhow::ensure!(
        s.written().await? == WRITTEN,
        "the settlement after it did not run the call"
    );
    Ok(())
}

// The adapter's events are instructions of the transaction (self-invoked),
// not log lines, so a log the runtime truncates keeps them all.
#[tokio::test(flavor = "multi_thread")]
async fn the_adapters_events_survive_a_truncated_log() -> anyhow::Result<()> {
    let mut s = setup().await?;
    // Agave keeps 10,000 bytes of program log per transaction, counting each
    // line with its "Program log: " prefix; the test forwarder's lines are
    // 100 bytes, so this many fill it before the settlement logs anything.
    let lines = 10_000_usize.div_ceil("Program log: ".len() + 100);
    s.register(
        vec![AccountMeta::new(s.account, false)],
        vec![log_ix(&s.forwarder, u8::try_from(lines)?)],
    );
    let tx = s.prove_call(5, write_input(&WRITTEN), &WRITTEN, 1).await?;
    let signature = s.env.protocol_adapter.settle(tx).await?;
    let executed = Executed::read(&s.env.protocol_adapter.rpc, &signature).await?;
    anyhow::ensure!(
        executed.logs.iter().any(|line| line == "Log truncated"),
        "the settlement's log is not truncated: {:?}",
        executed.logs
    );
    // Every event of the settlement, in pa-evm's order: the call, the
    // action, the new root, the transaction.
    let events = s.events(&executed)?;
    anyhow::ensure!(
        matches!(
            events.as_slice(),
            [
                PaEvent::ForwarderCallExecuted(call),
                PaEvent::ActionExecuted(_),
                PaEvent::CommitmentTreeRootAdded(_),
                PaEvent::TransactionExecuted(_),
            ] if call.output == WRITTEN
        ),
        "the settlement's events are {events:?}, not its call's, its action's, its root's and \
         its transaction's"
    );
    Ok(())
}

// As pa-evm's ActionExecuted, the event names each resource's logic.
#[tokio::test(flavor = "multi_thread")]
async fn the_action_executed_event_carries_its_resources_logic_refs() -> anyhow::Result<()> {
    let mut s = setup().await?;
    s.register(vec![AccountMeta::new(s.account, false)], vec![]);
    let tx = s.prove_call(6, write_input(&WRITTEN), &WRITTEN, 1).await?;
    let signature = s.env.protocol_adapter.settle(tx).await?;
    let executed = Executed::read(&s.env.protocol_adapter.rpc, &signature).await?;
    let events = s.events(&executed)?;
    let [action] = events
        .iter()
        .filter_map(|event| match event {
            PaEvent::ActionExecuted(action) => Some(action),
            _ => None,
        })
        .collect::<Vec<_>>()[..]
    else {
        anyhow::bail!("the settlement does not emit one ActionExecuted: {events:?}");
    };
    let logic: [u8; 32] = PASSTHROUGH_LOGIC_VK.into();
    anyhow::ensure!(
        action.consumed_logic_refs == [logic] && action.created_logic_refs == [logic],
        "the event names the consumed logic refs {:02x?} and created {:02x?}, not the \
         pass-through logic {logic:02x?}",
        action.consumed_logic_refs,
        action.created_logic_refs
    );
    Ok(())
}

// The largest settlement of this suite (a relayed write, with an instruction
// before it), sent as a v0 transaction through the settlement lookup table,
// fits one packet, and every account the same for every such settlement is
// looked up: the accounts two settlements share are the deployment's and the
// forwarder's, the ones they do not (the upload, the nullifiers, the new
// root's marker) are each settlement's own.
#[tokio::test(flavor = "multi_thread")]
async fn the_largest_settlement_fits_one_packet_with_its_fixed_accounts_looked_up()
-> anyhow::Result<()> {
    let mut s = setup().await?;
    s.register(
        vec![
            AccountMeta::new_readonly(s.forwarder, false),
            AccountMeta::new(s.account, false),
        ],
        vec![log_ix(&s.forwarder, 1)],
    );
    // As a deployment's table holds a forwarder's fixed accounts.
    s.env
        .protocol_adapter
        .extend_lookup_table(vec![s.forwarder, s.account])
        .await?;
    let input = relay_input(PASSTHROUGH_LOGIC_VK.into(), &write_input(&WRITTEN));

    let mut settled = Vec::new();
    for seed in [7, 8] {
        let tx = s.prove_call(seed, input.clone(), &[RELAY_OK], 2).await?;
        let signature = s.env.protocol_adapter.settle(tx).await?;
        let executed = Executed::read(&s.env.protocol_adapter.rpc, &signature).await?;
        let size = bincode::serialize(&executed.transaction)?.len();
        let message = &executed.transaction.message;
        println!(
            "settlement {seed} as v0: {size} bytes, {} static keys, {} looked up",
            message.static_account_keys().len(),
            executed.loaded.len()
        );
        anyhow::ensure!(
            size <= solana_packet::PACKET_DATA_SIZE,
            "settlement {seed} is {size} bytes, more than one packet"
        );
        settled.push(executed);
    }

    let keys = |executed: &Executed| -> Vec<Pubkey> {
        let message = &executed.transaction.message;
        message
            .static_account_keys()
            .iter()
            .chain(&executed.loaded)
            .copied()
            .collect()
    };
    let other = keys(&settled[1]);
    let message = &settled[0].transaction.message;
    let invoked: Vec<Pubkey> = message
        .instructions()
        .iter()
        .map(|ix| message.static_account_keys()[usize::from(ix.program_id_index)])
        .collect();
    let signers =
        &message.static_account_keys()[..usize::from(message.header().num_required_signatures)];
    for key in keys(&settled[0]) {
        if !other.contains(&key) || invoked.contains(&key) || signers.contains(&key) {
            continue;
        }
        anyhow::ensure!(
            settled[0].loaded.contains(&key),
            "{key}, an account of every such settlement, is static, not looked up: the \
             settlement lookup table lacks it"
        );
    }
    Ok(())
}
