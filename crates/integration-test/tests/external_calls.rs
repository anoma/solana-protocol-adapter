//! The adapter's external calls, made to the test forwarder from a
//! pass-through action: the accounts a forwarder receives, its output checked
//! against the proof's, the instructions a settlement transaction carries
//! before the settlement, the events the adapter emits, and the settlement's
//! size.

use std::sync::Arc;

use anchor_lang::Discriminator;
use anoma_pa_solana_client::cpi_event_discriminator;
use anoma_pa_solana_client::external_call::{OutputMode, SolanaExternalCall};
use anoma_pa_solana_client::forwarders::{CallAccountsFuture, Forwarder};
use anoma_pa_solana_client::generated::protocol_adapter::client::args;
use anoma_pa_solana_client::generated::protocol_adapter::events::{
    ActionExecuted, CommitmentTreeRootAdded, ForwarderCallExecuted, TransactionExecuted,
};
use anoma_pa_solana_client::settlement::CallAccounts;
use anoma_pa_solana_integration_test::envs::local::Environment as SolanaLocalEnv;
use anoma_pa_solana_integration_test::executed::Executed;
use anoma_pa_solana_integration_test::test_forwarder::{
    RELAY_OK, log_ix, relay_input, write_input,
};
use anoma_pa_testkit::environment::Refusal;
use anoma_pa_testkit::fixtures::passthrough::{self, PASSTHROUGH_LOGIC_VK};
use anoma_pa_testkit::prove_actions;
use anoma_pa_testkit::transaction::Transaction;
use anoma_rm_risc0::utils::bytes_to_words;
use anyhow::Context;
use solana_instruction::{AccountMeta, Instruction};
use solana_keypair::Keypair;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_signer::Signer;
use surfpool_sdk::Pubkey;

/// What the write calls write, and what the account holds before.
const WRITTEN: [u8; 4] = [1, 2, 3, 4];
const UNWRITTEN: [u8; 4] = [0; 4];

/// A forwarder whose every call takes the same segment, with the same
/// instructions preceding the settlement.
struct Fixed {
    segment: Vec<AccountMeta>,
    preceding: Vec<Instruction>,
}

impl Forwarder for Fixed {
    fn call_accounts<'a>(
        &'a self,
        _rpc: &'a RpcClient,
        calls: &'a [SolanaExternalCall],
        _preceding_start: usize,
    ) -> CallAccountsFuture<'a> {
        Box::pin(async move {
            Ok(CallAccounts {
                segments: vec![self.segment.clone(); calls.len()],
                preceding: self.preceding.clone(),
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
    /// Proves a pass-through action, with nonces from `seed`, that calls the
    /// test forwarder with `input`, expecting `expected`, and has the
    /// forwarder's calls take `segment` after the forwarder, with `preceding`
    /// before the settlement.
    async fn prove_call(
        &mut self,
        seed: u8,
        segment: Vec<AccountMeta>,
        preceding: Vec<Instruction>,
        input: Vec<u8>,
        expected: &[u8],
    ) -> anyhow::Result<Transaction> {
        let segment: Vec<AccountMeta> =
            std::iter::once(AccountMeta::new_readonly(self.forwarder, false))
                .chain(segment)
                .collect();
        let call = SolanaExternalCall {
            program_id: self.forwarder.to_bytes(),
            instruction_data: input,
            expected_output: expected.to_vec(),
            output_mode: OutputMode::ReturnData,
            num_accounts: u8::try_from(segment.len())?,
        };
        self.env
            .protocol_adapter
            .forwarders
            .register(self.forwarder, Arc::new(Fixed { segment, preceding }));
        let action = passthrough::build(
            seed,
            vec![bytes_to_words(&call.encode())],
            passthrough::Overrides::default(),
        )?;
        prove_actions(&self.env, &[action.witnesses]).await
    }

    /// A call that writes `WRITTEN` to the account, which it passes writable,
    /// and expects it back.
    async fn prove_write(
        &mut self,
        seed: u8,
        preceding: Vec<Instruction>,
    ) -> anyhow::Result<Transaction> {
        let segment = vec![AccountMeta::new(self.account, false)];
        self.prove_call(seed, segment, preceding, write_input(&WRITTEN), &WRITTEN)
            .await
    }

    /// That write, relayed by the test forwarder to the program after it in
    /// the segment: itself.
    async fn prove_relayed_write(
        &mut self,
        seed: u8,
        preceding: Vec<Instruction>,
    ) -> anyhow::Result<Transaction> {
        let segment = vec![
            AccountMeta::new_readonly(self.forwarder, false),
            AccountMeta::new(self.account, false),
        ];
        let input = relay_input(PASSTHROUGH_LOGIC_VK.into(), &write_input(&WRITTEN));
        self.prove_call(seed, segment, preceding, input, &[RELAY_OK])
            .await
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

    /// The adapter's events of type `E` in the settlement `executed`.
    fn events<E: Discriminator + anchor_lang::AnchorDeserialize>(
        &self,
        executed: &Executed,
    ) -> anyhow::Result<Vec<E>> {
        executed.events(&self.env.protocol_adapter.program)
    }
}

// The segment's accounts reach the forwarder with the writability the
// submitter gives them, so a forwarder can change the state its call names.
#[tokio::test(flavor = "multi_thread")]
async fn a_forwarder_writes_an_account_its_segment_passes_writable() -> anyhow::Result<()> {
    let mut s = setup().await?;
    let tx = s.prove_write(1, vec![]).await?;
    s.env.protocol_adapter.settled(tx).await?;
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
    let segment = vec![AccountMeta::new(s.account, false)];
    let tx = s
        .prove_call(2, segment, vec![], write_input(&WRITTEN), &[9; 4])
        .await?;
    let refusal = s.env.protocol_adapter.submit(tx).await?.err();
    anyhow::ensure!(
        refusal == Some(Refusal::ExternalCallOutputMismatch),
        "the adapter returned {refusal:?}, not a refusal for the output"
    );
    let written = s.written().await?;
    anyhow::ensure!(
        written == UNWRITTEN,
        "the refused settlement left the account holding {written:?}"
    );
    Ok(())
}

// A refused settlement whose log the runtime truncated is not decoded: the
// truncation may have dropped the deepest failure's line, so the harness
// reports the failure instead of naming a refusal.
#[tokio::test(flavor = "multi_thread")]
async fn a_refusal_with_a_truncated_log_is_reported_not_named() -> anyhow::Result<()> {
    let mut s = setup().await?;
    let lines = 10_000_usize.div_ceil("Program log: ".len() + 100);
    let flood = log_ix(&s.forwarder, u8::try_from(lines)?);
    let segment = vec![AccountMeta::new(s.account, false)];
    let tx = s
        .prove_call(9, segment, vec![flood], write_input(&WRITTEN), &[9; 4])
        .await?;
    let Err(error) = s.env.protocol_adapter.submit(tx).await else {
        anyhow::bail!("the harness decoded a refusal from a truncated log");
    };
    anyhow::ensure!(
        format!("{error:#}").contains("the runtime truncated the log"),
        "the failure does not name the truncation: {error:#}"
    );
    Ok(())
}

// A segment can name more than one program: the test forwarder relays the
// call to the program after it (itself, which writes the account), and that
// program's call takes the accounts after it.
#[tokio::test(flavor = "multi_thread")]
async fn a_forwarder_calls_a_second_program_its_segment_names() -> anyhow::Result<()> {
    let mut s = setup().await?;
    let tx = s.prove_relayed_write(3, vec![]).await?;
    let executed = s.env.protocol_adapter.settled(tx).await?;
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
    let tx = s.prove_write(4, vec![preceding.clone()]).await?;
    let executed = s.env.protocol_adapter.settled(tx).await?;
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
    let flood = log_ix(&s.forwarder, u8::try_from(lines)?);
    let tx = s.prove_write(5, vec![flood]).await?;
    let executed = s.env.protocol_adapter.settled(tx).await?;
    anyhow::ensure!(
        executed.logs.iter().any(|line| line == "Log truncated"),
        "the settlement's log is not truncated: {:?}",
        executed.logs
    );
    // Every event of the settlement, in pa-evm's order: the call, the
    // action, the new root, the transaction.
    let order = executed
        .cpi_events(&s.env.protocol_adapter.program)
        .map(cpi_event_discriminator)
        .collect::<Result<Vec<_>, _>>()?;
    let expected = [
        ForwarderCallExecuted::DISCRIMINATOR,
        ActionExecuted::DISCRIMINATOR,
        CommitmentTreeRootAdded::DISCRIMINATOR,
        TransactionExecuted::DISCRIMINATOR,
    ];
    anyhow::ensure!(
        order.iter().map(|d| d.as_slice()).eq(expected),
        "the settlement's events have the discriminators {order:02x?}, not its call's, its \
         action's, its root's and its transaction's {expected:02x?}"
    );
    let calls = s.events::<ForwarderCallExecuted>(&executed)?;
    anyhow::ensure!(
        matches!(calls.as_slice(), [call] if call.output == WRITTEN),
        "the settlement's calls are {calls:?}, not one returning {WRITTEN:?}"
    );
    Ok(())
}

// As pa-evm's ActionExecuted, the event names each resource's logic.
#[tokio::test(flavor = "multi_thread")]
async fn the_action_executed_event_carries_its_resources_logic_refs() -> anyhow::Result<()> {
    let mut s = setup().await?;
    let tx = s.prove_write(6, vec![]).await?;
    let executed = s.env.protocol_adapter.settled(tx).await?;
    let actions = s.events::<ActionExecuted>(&executed)?;
    let [action] = &actions[..] else {
        anyhow::bail!("the settlement does not emit one ActionExecuted: {actions:?}");
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
// before it) settles in one v1 transaction: the instruction before it, then
// the adapter's `settle` carrying the input.
#[tokio::test(flavor = "multi_thread")]
async fn the_largest_settlement_settles_in_one_v1_transaction() -> anyhow::Result<()> {
    let mut s = setup().await?;
    let tx = s
        .prove_relayed_write(7, vec![log_ix(&s.forwarder, 1)])
        .await?;
    let executed = s.env.protocol_adapter.settled(tx).await?;

    let solana_message::VersionedMessage::V1(message) = &executed.transaction.message else {
        anyhow::bail!("the settlement is not a v1 transaction");
    };
    let size = anoma_pa_solana_client::transaction_size(message);
    println!(
        "the largest settlement: {size} bytes, {} accounts",
        message.account_keys.len()
    );
    let data = executed
        .instruction_data(&s.env.protocol_adapter.program)
        .context("the settlement transaction has no adapter instruction")?;
    anyhow::ensure!(
        data[..8] == *args::Settle::DISCRIMINATOR,
        "the largest settlement is not settled inline in one transaction"
    );
    Ok(())
}
