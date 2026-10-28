use std::sync::Arc;

use anchor_lang::{AnchorDeserialize, Discriminator};
use anoma_pa_solana_client::forwarders::Forwarders;
use anoma_pa_solana_client::generated::U256LeBytes;
use anoma_pa_solana_client::generated::protocol_adapter::accounts::PAStateAccount;
use anoma_pa_solana_client::generated::protocol_adapter::error::ProtocolAdapterError;
use anoma_pa_solana_client::generated::protocol_adapter::events::{
    ActionExecuted, ApplicationPayload, CommitmentTreeRootAdded, DiscoveryPayload, ExternalPayload,
    ForwarderCallExecuted, ResourcePayload, TransactionExecuted,
};
use anoma_pa_solana_client::generated::protocol_adapter::types;
use anoma_pa_solana_client::instance;
use anoma_pa_solana_client::settlement::SettlementPlan;
use anoma_pa_solana_client::submission::{
    Failure, SimulationOutcome, Submission, read_adapter_state, read_verifier_program,
    settlement_failure,
};
use anoma_pa_solana_client::{
    TXDATA_EXPIRY_SLOTS_DEFAULT, cpi_event_discriminator, decode_cpi_event,
};
use anoma_pa_testkit::commitment_tree::FrontierCommitmentTree;
use anoma_pa_testkit::environment::{
    DeniedLogicRef, Event, Outcome, PayloadKind, ProtocolAdapter as CoreProtocolAdapter, Refusal,
};
use anoma_pa_testkit::transaction::Transaction;
use anoma_rm_risc0::Digest;
use anyhow::Context;
use solana_instruction::Instruction;
use solana_keypair::Keypair;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_rpc_client_api::client_error::{Error as ClientError, ErrorKind};
use solana_rpc_client_api::request::{RpcError, RpcResponseErrorData};
use solana_rpc_client_api::response::RpcSimulateTransactionResult;
use solana_signature::Signature;
use solana_signer::Signer;
use surfpool_sdk::Pubkey;

use super::live_cluster::{LiveCluster, ensure_discriminator};
use super::runtime::{latest_blockhash, send_planned, send_signed};
use crate::executed::Executed;

/// The Solana protocol adapter, settling each transaction in one v1
/// transaction when it fits, through a transaction-data upload otherwise.
pub struct ProtocolAdapter {
    /// The runtime's RPC client, for reads: what it sends needs a signer,
    /// and the payer's keypair, which owns the adapter on a runtime of the
    /// harness's own, is handed out only there (`payer`).
    pub rpc: Arc<RpcClient>,
    /// Pays for and signs every upload and settlement, and the owner's calls.
    payer: Arc<Keypair>,
    pub program: Pubkey,
    /// The verifier program the router's entry for the adapter's selector
    /// names.
    pub verifier_program: Pubkey,
    pub commitment_tree: FrontierCommitmentTree,
    /// The forwarders the settled transactions call, which supply each
    /// call's accounts.
    pub forwarders: Forwarders,
    /// The next transaction-data upload's id; each settlement takes one.
    next_upload_id: u64,
    /// On a live cluster, what it is sent (`refuse_on_live_cluster`);
    /// `None` on a runtime of the harness's own, which is sent anything.
    live_cluster: Option<LiveCluster>,
}

impl ProtocolAdapter {
    /// The initialized adapter `program`: the verifier its router and
    /// selector name, and the commitment tree its state holds; `live` when it
    /// is a live cluster's.
    pub(in crate::envs) async fn new(
        rpc: Arc<RpcClient>,
        payer: Arc<Keypair>,
        program: Pubkey,
        live: bool,
    ) -> anyhow::Result<Self> {
        let state = read_adapter_state(&rpc, &program).await?;
        let verifier_program =
            read_verifier_program(&rpc, &state.verifier_router, state.proof_selector).await?;
        let adapter = Self {
            rpc,
            payer,
            program,
            verifier_program,
            commitment_tree: crate::commitment_tree::from_state(&state)?,
            forwarders: Forwarders::default(),
            next_upload_id: 0,
            live_cluster: live.then(|| LiveCluster::new(program)),
        };
        adapter.ensure_latest_root(&state)?;
        Ok(adapter)
    }

    /// Refuses `instructions` on a live cluster when one is not on its
    /// allowlist (`live_cluster`), before anything is sent: settlements, the
    /// payer's system, token and associated-token instructions, compute
    /// budgets, ed25519 checks, and the instructions registered with
    /// `allow_on_live_cluster`. Elsewhere every instruction is sent.
    pub(in crate::envs) fn refuse_on_live_cluster(
        &self,
        instructions: &[Instruction],
    ) -> anyhow::Result<()> {
        match &self.live_cluster {
            Some(live_cluster) => live_cluster.check(instructions),
            None => Ok(()),
        }
    }

    /// Allows a live cluster to be sent the instructions of `program` whose
    /// data starts with `discriminator` (an Anchor instruction's
    /// discriminator): a consumer's own instruction its tests need on a live
    /// cluster, such as one the payer makes for itself. Every other
    /// instruction of `program` stays refused, and an empty discriminator,
    /// which every instruction starts with, is an error. A runtime of the
    /// harness's own is sent every instruction already.
    pub fn allow_on_live_cluster(
        &mut self,
        program: Pubkey,
        discriminator: &[u8],
    ) -> anyhow::Result<()> {
        match &mut self.live_cluster {
            Some(live_cluster) => live_cluster.register(program, discriminator),
            None => ensure_discriminator(&program, discriminator),
        }
    }

    /// The adapter as the client's instance of it: its reads, and its
    /// instructions built for its address.
    pub fn instance(&self) -> instance::ProtocolAdapter<'_> {
        instance::ProtocolAdapter::at(&self.rpc, self.program)
    }

    /// The adapter's state account, as it is now.
    pub async fn state(&self) -> anyhow::Result<PAStateAccount> {
        Ok(read_adapter_state(&self.rpc, &self.program).await?)
    }

    /// Checks that the tree gives the root the adapter stores.
    fn ensure_latest_root(&self, state: &PAStateAccount) -> anyhow::Result<()> {
        let root = self.commitment_tree.root();
        anyhow::ensure!(
            root.as_bytes() == state.root,
            "the tree gives the root {root}, the adapter stores {}",
            Digest::from_bytes(state.root)
        );
        Ok(())
    }

    /// Submits `transaction` the way every submitter does (in one v1
    /// transaction when it fits, otherwise its data uploaded, the settlement,
    /// the upload closed), and returns the settlement as the runtime recorded
    /// it, or why the adapter refused it.
    pub async fn submit(
        &mut self,
        transaction: Transaction,
    ) -> anyhow::Result<Result<Executed, Refusal>> {
        // One blockhash serves every transaction of the settlement.
        let (submission, slot, blockhash) = futures::try_join!(
            self.prepare(transaction),
            async {
                self.rpc
                    .get_slot()
                    .await
                    .context("failed to fetch the slot")
            },
            latest_blockhash(&self.rpc),
        )?;
        self.ensure_latest_root(submission.state())?;

        let upload_id = self.next_upload_id;
        self.next_upload_id += 1;
        let plan = submission.plan_settlement(upload_id, slot + TXDATA_EXPIRY_SLOTS_DEFAULT)?;
        self.refuse_on_live_cluster(&planned_instructions(&plan))?;

        let (rpc, payer) = (&*self.rpc, &*self.payer);
        let settled = match &plan.upload {
            None => send_planned(rpc, payer, &plan.settle, blockhash).await,
            Some(upload) => {
                send_planned(rpc, payer, &upload.init, blockhash)
                    .await
                    .context("failed to create the transaction-data upload")?;
                let settled = async {
                    // Each write names its offset, so the chunks land in any order.
                    futures::future::try_join_all(upload.writes.iter().enumerate().map(
                        |(i, write)| async move {
                            send_planned(rpc, payer, write, blockhash)
                                .await
                                .with_context(|| format!("failed to write chunk {i} of the upload"))
                        },
                    ))
                    .await?;
                    send_planned(rpc, payer, &plan.settle, blockhash).await
                }
                .await;
                // The upload is closed whatever the settlement's outcome, so a
                // refused settlement does not strand its rent.
                send_planned(rpc, payer, &upload.close, blockhash)
                    .await
                    .context("failed to close the transaction-data upload")?;
                settled
            }
        };
        let signature = match settled {
            Ok(signature) => signature,
            Err(error) => {
                return match self.refusal(&error) {
                    Ok(Some(refusal)) => Ok(Err(refusal)),
                    Ok(None) => Err(error.context("protocol adapter settlement failed")),
                    Err(undecodable) => Err(error
                        .context(format!("{undecodable:#}"))
                        .context("protocol adapter settlement failed")),
                };
            }
        };

        self.commitment_tree.add(
            submission
                .resources()
                .created
                .iter()
                .copied()
                .map(Digest::from_bytes),
        );
        let (state, executed) = futures::try_join!(self.state(), Executed::read(rpc, &signature))?;
        self.ensure_latest_root(&state)?;
        Ok(Ok(executed))
    }

    /// Submits `transaction`, which the adapter must settle, and returns the
    /// settlement as the runtime recorded it.
    pub async fn settled(&mut self, transaction: Transaction) -> anyhow::Result<Executed> {
        self.submit(transaction).await?.map_err(|refusal| {
            anyhow::anyhow!("the protocol adapter refused the transaction: {refusal:?}")
        })
    }

    /// `transaction` prepared to settle: the adapter's state and its calls'
    /// accounts from the registered forwarders; the payer pays.
    async fn prepare(&self, transaction: Transaction) -> anyhow::Result<Submission> {
        Ok(Submission::prepare(
            &self.rpc,
            &self.forwarders,
            self.program,
            self.payer.pubkey(),
            self.state().await?,
            self.verifier_program,
            transaction.into_arm(),
        )
        .await?)
    }

    /// Simulates settling `transaction` (pa-evm's `simulateExecute`) in the
    /// runtime's simulation, which changes nothing and checks no signature,
    /// skipping the RISC Zero proof's verification when
    /// `skip_risc_zero_proof_verification`. An unproven transaction (its
    /// aggregation proof empty) carries the adapter's proof selector instead.
    /// Returns the compute units the simulation used when the transaction
    /// would execute, otherwise the adapter's error.
    pub async fn simulate(
        &self,
        transaction: Transaction,
        skip_risc_zero_proof_verification: bool,
    ) -> anyhow::Result<Result<u64, ProtocolAdapterError>> {
        let outcome = self
            .prepare(transaction)
            .await?
            .simulate(&self.rpc, skip_risc_zero_proof_verification)
            .await?;
        Ok(match outcome {
            SimulationOutcome::Executable { units_consumed } => {
                Ok(units_consumed.context("the simulation reports no compute units")?)
            }
            SimulationOutcome::Failed(Failure::Adapter(error)) => Err(error),
            other => anyhow::bail!("the simulation did not end in an adapter error: {other:?}"),
        })
    }

    /// Why the adapter refused the settlement that failed with `error`, from
    /// the runtime's simulation of it (`refusal_in_logs`). `None` when the
    /// failure is not one of the protocol's refusals; an error when the
    /// simulation's log cannot say.
    fn refusal(&self, error: &anyhow::Error) -> anyhow::Result<Option<Refusal>> {
        let Some(ErrorKind::RpcError(RpcError::RpcResponseError {
            data:
                RpcResponseErrorData::SendTransactionPreflightFailure(RpcSimulateTransactionResult {
                    logs: Some(logs),
                    ..
                }),
            ..
        })) = error.downcast_ref::<ClientError>().map(ClientError::kind)
        else {
            return Ok(None);
        };
        refusal_in_logs(logs, &self.program, &self.verifier_program)
    }

    /// Sends `instruction`, which the adapter's owner signs: the payer, which
    /// owns the adapter.
    async fn as_owner(&self, instruction: Instruction, what: &str) -> anyhow::Result<()> {
        self.send(&[instruction], &[])
            .await
            .with_context(|| format!("failed to {what}"))?;
        Ok(())
    }

    /// Sends `instructions` as one transaction the payer pays for, signed by
    /// it and `signers`, once the live cluster's allowlist passes them
    /// (`refuse_on_live_cluster`).
    pub(in crate::envs) async fn send(
        &self,
        instructions: &[Instruction],
        signers: &[&Keypair],
    ) -> anyhow::Result<Signature> {
        self.refuse_on_live_cluster(instructions)?;
        send_signed(&self.rpc, &self.payer, signers, instructions).await
    }

    /// The payer's address.
    pub fn payer_pubkey(&self) -> Pubkey {
        self.payer.pubkey()
    }

    /// The payer's keypair, for a test's own signing on a runtime of the
    /// harness's own; an error on a live cluster, where the payer holds the
    /// adapter's authority and everything it signs goes through the
    /// allowlist (`Environment::send`).
    pub fn payer(&self) -> anyhow::Result<&Arc<Keypair>> {
        anyhow::ensure!(
            self.live_cluster.is_none(),
            "the payer's keypair is not handed out on a live cluster"
        );
        Ok(&self.payer)
    }
}

impl CoreProtocolAdapter for ProtocolAdapter {
    async fn settle(&mut self, transaction: Transaction) -> anyhow::Result<Outcome> {
        Ok(match self.submit(transaction).await? {
            Ok(executed) => Outcome::Settled(
                executed
                    .cpi_events(&self.program)
                    .map(settlement_event)
                    .collect::<anyhow::Result<_>>()?,
            ),
            Err(refusal) => Outcome::Refused(refusal),
        })
    }

    async fn commitment_tree(&self) -> anyhow::Result<FrontierCommitmentTree> {
        crate::commitment_tree::from_state(&self.state().await?)
    }

    async fn latest_root(&self) -> anyhow::Result<Digest> {
        Ok(Digest::from_bytes(self.state().await?.root))
    }

    async fn set_kind_tree_root(&mut self, root: Digest) -> anyhow::Result<()> {
        let ix = self
            .instance()
            .set_kind_tree_root_ix(&self.payer.pubkey(), root.into());
        self.as_owner(ix, "set the kind tree root").await
    }

    async fn pause(&mut self) -> anyhow::Result<()> {
        let ix = self.instance().pause_ix(&self.payer.pubkey());
        self.as_owner(ix, "pause the protocol adapter").await
    }

    async fn unpause(&mut self) -> anyhow::Result<()> {
        let ix = self.instance().unpause_ix(&self.payer.pubkey());
        self.as_owner(ix, "unpause the protocol adapter").await
    }

    async fn deny_logic_refs(&mut self, logic_refs: &[DeniedLogicRef]) -> anyhow::Result<()> {
        let entries: Vec<_> = logic_refs
            .iter()
            .map(|entry| types::DeniedLogicRef {
                logic_ref: entry.logic_ref.into(),
                consumed: entry.consumed,
            })
            .collect();
        let ix = self
            .instance()
            .deny_logic_refs_ix(&self.payer.pubkey(), &entries);
        self.as_owner(ix, "deny the logic refs").await
    }
}

/// Every instruction of the transactions `plan` sends: the upload's, and the
/// settlement's with the instructions the forwarders put before it.
fn planned_instructions(plan: &SettlementPlan) -> Vec<Instruction> {
    let upload = plan.upload.iter().flat_map(|upload| {
        std::iter::once(&upload.init)
            .chain(&upload.writes)
            .chain(std::iter::once(&upload.close))
    });
    upload
        .chain(std::iter::once(&plan.settle))
        .flat_map(|transaction| transaction.instructions.iter().cloned())
        .collect()
}

/// The refusal a settlement's log names, from the failure that ended it
/// (`settlement_failure`): the verifier refusing the aggregation proof, or
/// the adapter's own error, the refusal its `ProtocolAdapterError` is.
/// `None` for any other failure.
fn refusal_in_logs(
    logs: &[String],
    program: &Pubkey,
    verifier: &Pubkey,
) -> anyhow::Result<Option<Refusal>> {
    Ok(match settlement_failure(logs, program, verifier)? {
        Some(Failure::ProofRefused { .. }) => Some(Refusal::InvalidAggregationProof),
        Some(Failure::Adapter(error)) => refusal_for(error),
        Some(Failure::Anchor { .. } | Failure::Program { .. }) | None => None,
    })
}

/// The refusal the adapter's `error` is, if it is one of the protocol's. The
/// match lists every error, so an error the client's `ProtocolAdapterError`
/// gains is mapped here before the harness builds.
fn refusal_for(error: ProtocolAdapterError) -> Option<Refusal> {
    match error {
        ProtocolAdapterError::EnforcedPause => Some(Refusal::Paused),
        ProtocolAdapterError::ResourceWithDeniedLogicRef => Some(Refusal::DeniedLogicRef),
        ProtocolAdapterError::UnacceptedKindTreeRoot => Some(Refusal::UnacceptedKindTreeRoot),
        ProtocolAdapterError::NonExistingRoot => Some(Refusal::UnknownRoot),
        ProtocolAdapterError::PreExistingNullifier => Some(Refusal::NullifierSpent),
        ProtocolAdapterError::ForwarderCallOutputMismatch => {
            Some(Refusal::ExternalCallOutputMismatch)
        }
        ProtocolAdapterError::ComplianceKeyMismatch | ProtocolAdapterError::InvalidProof => {
            Some(Refusal::InvalidAggregationProof)
        }
        ProtocolAdapterError::NullifierPdaMismatch
        | ProtocolAdapterError::RootPdaMismatch
        | ProtocolAdapterError::TxDataExpired
        | ProtocolAdapterError::TxDataBoundsExceeded
        | ProtocolAdapterError::TxDataExpiryTooSoon
        | ProtocolAdapterError::TxDataExpiryTooLate
        | ProtocolAdapterError::TxDataExtendMustIncrease
        | ProtocolAdapterError::TxDataNotExpired
        | ProtocolAdapterError::InvalidExpiryConfig
        | ProtocolAdapterError::InvalidTransactionData
        | ProtocolAdapterError::EmptyTransactionNotAllowed
        | ProtocolAdapterError::VerifierRouterFailed
        | ProtocolAdapterError::AggregationRequired
        | ProtocolAdapterError::RiscZeroVerifierSelectorMismatch
        | ProtocolAdapterError::InvalidExternalCallBlob
        | ProtocolAdapterError::UnregisteredForwarder
        | ProtocolAdapterError::ExternalCallCpiFailed
        | ProtocolAdapterError::DeltaProofVerificationFailed
        | ProtocolAdapterError::DeltaMismatch
        | ProtocolAdapterError::InvalidDeltaProof
        | ProtocolAdapterError::PointNotOnCurve
        | ProtocolAdapterError::ExpectedDeltaProof
        | ProtocolAdapterError::ZeroKindTreeRootNotAllowed
        | ProtocolAdapterError::Unauthorized
        | ProtocolAdapterError::ExpectedPause
        | ProtocolAdapterError::RiscZeroVerifierPaused
        | ProtocolAdapterError::InvalidVerifierEntry
        | ProtocolAdapterError::TreeMaxDepthReached
        | ProtocolAdapterError::InvalidMarker
        | ProtocolAdapterError::MarkerUnexpectedOwner
        | ProtocolAdapterError::MarkerUnexpectedData
        | ProtocolAdapterError::PreExistingRoot
        | ProtocolAdapterError::UnsupportedStateSchema
        | ProtocolAdapterError::ZeroLogicRefNotAllowed
        | ProtocolAdapterError::LogicRefAlreadyDenied
        | ProtocolAdapterError::ZeroRiscZeroVerifierRouterNotAllowed
        | ProtocolAdapterError::ZeroRiscZeroVerifierSelectorNotAllowed
        | ProtocolAdapterError::OwnableUnauthorizedAccount
        | ProtocolAdapterError::OwnableInvalidOwner
        | ProtocolAdapterError::InvalidUpgradeBuffer
        | ProtocolAdapterError::Simulated => None,
    }
}

/// A settlement's event as pa-testkit's, from `data`, the instruction data of
/// one of the adapter's event self-invocations: the adapter's settlement
/// events, which mirror pa-evm's. An owner's event is no settlement's.
fn settlement_event(data: &[u8]) -> anyhow::Result<Event> {
    fn decoded<E: Discriminator + AnchorDeserialize>(data: &[u8]) -> anyhow::Result<Option<E>> {
        decode_cpi_event::<E>(data)
            .with_context(|| format!("the adapter's event {data:02x?} does not decode"))
    }
    let payload = |kind, tag, U256LeBytes(index), blob| {
        let (low, high) = index.split_at(size_of::<u32>());
        anyhow::ensure!(
            high.iter().all(|byte| *byte == 0),
            "a payload's index {index:02x?} (little-endian) exceeds u32"
        );
        Ok(Event::Payload {
            kind,
            tag: Digest::from_bytes(tag),
            index: u32::from_le_bytes(low.try_into()?),
            blob,
        })
    };
    let digests = |values: Vec<[u8; 32]>| values.into_iter().map(Digest::from_bytes).collect();
    if decoded::<ForwarderCallExecuted>(data)?.is_some() {
        return Ok(Event::ForwarderCallExecuted);
    }
    if let Some(p) = decoded::<ResourcePayload>(data)? {
        return payload(PayloadKind::Resource, p.tag, p.index, p.blob);
    }
    if let Some(p) = decoded::<DiscoveryPayload>(data)? {
        return payload(PayloadKind::Discovery, p.tag, p.index, p.blob);
    }
    if let Some(p) = decoded::<ExternalPayload>(data)? {
        return payload(PayloadKind::External, p.tag, p.index, p.blob);
    }
    if let Some(p) = decoded::<ApplicationPayload>(data)? {
        return payload(PayloadKind::Application, p.tag, p.index, p.blob);
    }
    if let Some(action) = decoded::<ActionExecuted>(data)? {
        return Ok(Event::ActionExecuted {
            action_tree_root: Digest::from_bytes(action.action_tree_root),
            nullifiers: digests(action.nullifiers),
            consumed_logic_refs: digests(action.consumed_logic_refs),
            commitments: digests(action.commitments),
            created_logic_refs: digests(action.created_logic_refs),
        });
    }
    if let Some(added) = decoded::<CommitmentTreeRootAdded>(data)? {
        return Ok(Event::CommitmentTreeRootAdded {
            root: Digest::from_bytes(added.root),
        });
    }
    if let Some(executed) = decoded::<TransactionExecuted>(data)? {
        return Ok(Event::TransactionExecuted {
            transaction_id: Digest::from_bytes(executed.transaction_id),
        });
    }
    anyhow::bail!(
        "a settlement emitted the adapter's event with the discriminator {:02x?}, which is no \
         settlement event",
        cpi_event_discriminator(data)?
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::executed::emitted;
    use anoma_pa_solana_client::SettlementAccounts;

    const ADAPTER: Pubkey = Pubkey::new_from_array([1; 32]);
    const VERIFIER: Pubkey = Pubkey::new_from_array([2; 32]);
    const FORWARDER: Pubkey = Pubkey::new_from_array([3; 32]);

    fn invoke(program: Pubkey, depth: u8) -> String {
        format!("Program {program} invoke [{depth}]")
    }

    /// The runtime's line for `program` failing with custom error `code`.
    fn failure(program: Pubkey, code: u32) -> String {
        format!("Program {program} failed: custom program error: {code:#x}")
    }

    fn refusal(logs: &[String]) -> Option<Refusal> {
        refusal_in_logs(logs, &ADAPTER, &VERIFIER).unwrap()
    }

    #[test]
    fn the_adapters_error_is_its_code() {
        let code = u32::from(ProtocolAdapterError::ForwarderCallOutputMismatch);
        let logs = [invoke(ADAPTER, 1), failure(ADAPTER, code)];
        assert_eq!(refusal(&logs), Some(Refusal::ExternalCallOutputMismatch));
    }

    #[test]
    fn the_verifiers_error_refuses_the_proof() {
        // 6600: the mock verifier's ClaimDigestMismatch.
        let logs = [
            invoke(ADAPTER, 1),
            invoke(VERIFIER, 2),
            failure(VERIFIER, 6600),
            failure(ADAPTER, 6600),
        ];
        assert_eq!(refusal(&logs), Some(Refusal::InvalidAggregationProof));
    }

    #[test]
    fn an_anchor_framework_error_of_the_adapter_is_no_refusal() {
        // 3012: Anchor's AccountNotInitialized, below the adapter's own codes.
        let logs = [invoke(ADAPTER, 1), failure(ADAPTER, 3012)];
        assert_eq!(refusal(&logs), None);
    }

    #[test]
    fn another_programs_failure_is_no_refusal() {
        let code = u32::from(ProtocolAdapterError::NonExistingRoot);
        let logs = [
            invoke(ADAPTER, 1),
            invoke(FORWARDER, 2),
            failure(FORWARDER, code),
            failure(ADAPTER, code),
        ];
        assert_eq!(refusal(&logs), None);
    }

    #[test]
    fn a_payload_event_is_pa_testkits_payload_at_its_index() {
        let mut index = [0; 32];
        index[0] = 3;
        let event = settlement_event(&emitted(&ExternalPayload {
            tag: [9; 32],
            index: U256LeBytes(index),
            blob: vec![1, 2],
        }))
        .unwrap();
        assert!(
            matches!(
                &event,
                Event::Payload { kind: PayloadKind::External, index: 3, blob, .. } if blob == &[1, 2]
            ),
            "an ExternalPayload at index 3 became {event:?}"
        );
    }

    #[test]
    fn a_payload_index_past_u32_is_refused() {
        let mut index = [0; 32];
        index[size_of::<u32>()] = 1;
        let err = settlement_event(&emitted(&ResourcePayload {
            tag: [9; 32],
            index: U256LeBytes(index),
            blob: vec![],
        }))
        .unwrap_err()
        .to_string();
        assert!(err.contains("exceeds u32"), "index 2^32: {err}");
    }

    #[test]
    fn an_owners_event_is_no_settlement_event() {
        use anoma_pa_solana_client::generated::protocol_adapter::events::Paused;
        let err = settlement_event(&emitted(&Paused {
            account: Pubkey::new_from_array([4; 32]),
        }))
        .unwrap_err()
        .to_string();
        println!("{err}");
        assert!(err.contains("which is no settlement event"), "{err}");
    }

    /// An adapter on a live cluster whose RPC endpoint nothing serves: a
    /// call that sent anything would fail to reach it.
    fn live_adapter() -> ProtocolAdapter {
        ProtocolAdapter {
            rpc: Arc::new(RpcClient::new("http://127.0.0.1:1".to_string())),
            payer: Arc::new(Keypair::new()),
            program: ADAPTER,
            verifier_program: VERIFIER,
            commitment_tree: FrontierCommitmentTree::new(0, vec![]).unwrap(),
            forwarders: Forwarders::default(),
            next_upload_id: 0,
            live_cluster: Some(LiveCluster::new(ADAPTER)),
        }
    }

    /// The adapter of `live_adapter` on a runtime of the harness's own.
    fn local_adapter() -> ProtocolAdapter {
        ProtocolAdapter {
            live_cluster: None,
            ..live_adapter()
        }
    }

    /// `result` failed as refused on a live cluster, whatever it adds of
    /// what it was doing.
    fn refused(name: &str, result: anyhow::Result<()>) {
        let error = format!("{:#}", result.expect_err(name));
        println!("{name}: {error}");
        assert!(
            error.ends_with(&format!("refused on a live cluster: {name}")),
            "{error}"
        );
    }

    #[tokio::test]
    async fn a_live_cluster_refuses_the_owners_calls_before_sending_them() {
        let mut adapter = live_adapter();
        refused(
            "protocol_adapter/set_kind_tree_root",
            adapter
                .set_kind_tree_root(Digest::from_bytes([7; 32]))
                .await,
        );
        refused("protocol_adapter/pause", adapter.pause().await);
        refused("protocol_adapter/unpause", adapter.unpause().await);
        refused(
            "protocol_adapter/deny_logic_refs",
            adapter
                .deny_logic_refs(&[DeniedLogicRef {
                    logic_ref: Digest::from_bytes([7; 32]),
                    consumed: true,
                }])
                .await,
        );
    }

    /// A settlement plan whose transactions carry `preceding` before the
    /// adapter's `settle`, uploading its input when `uploads`.
    fn plan(preceding: Instruction, uploads: bool) -> SettlementPlan {
        use anoma_pa_solana_client::settlement::{PlannedTransaction, Upload};
        let adapter = live_adapter();
        let instance = adapter.instance();
        let key = Pubkey::new_from_array([5; 32]);
        let transaction = |instructions| PlannedTransaction {
            instructions,
            config: Default::default(),
        };
        let settle = instance.settle_ix(
            &key,
            &[1],
            SettlementAccounts {
                new_root_marker: None,
                verifier_router_program: key,
                router: key,
                verifier_entry: key,
                verifier_program: key,
                remaining_accounts: vec![],
            },
        );
        SettlementPlan {
            upload: uploads.then(|| Upload {
                init: transaction(vec![instance.txdata_init_ix(&key, 0, 1, 2)]),
                writes: vec![transaction(vec![instance.txdata_write_ix(
                    &key,
                    0,
                    0,
                    &[1],
                )])],
                close: transaction(vec![instance.txdata_close_ix(&key, &key, 0)]),
            }),
            settle: transaction(vec![preceding, settle]),
            new_root: None,
        }
    }

    #[test]
    fn a_live_cluster_checks_every_instruction_a_settlement_sends() {
        let adapter = live_adapter();
        let check =
            |plan: &SettlementPlan| adapter.refuse_on_live_cluster(&planned_instructions(plan));
        let ed25519 =
            Instruction::new_with_bytes(solana_sdk_ids::ed25519_program::ID, &[1], vec![]);
        let preceding = Instruction::new_with_bytes(FORWARDER, &[8; 8], vec![]);
        for uploads in [false, true] {
            let planned = planned_instructions(&plan(ed25519.clone(), uploads));
            assert_eq!(planned.len(), if uploads { 5 } else { 2 }, "{planned:?}");
            check(&plan(ed25519.clone(), uploads))
                .expect("a settlement after an ed25519 check is sent");
            refused(
                &format!("{FORWARDER}/[08, 08, 08, 08, 08, 08, 08, 08]"),
                check(&plan(preceding.clone(), uploads)),
            );
        }
    }

    #[test]
    fn a_runtime_of_the_harnesss_own_is_sent_the_owners_calls() {
        let local = local_adapter();
        local
            .refuse_on_live_cluster(&[local.instance().pause_ix(&ADAPTER)])
            .expect("a runtime of the harness's own is sent the owner's calls");
    }

    #[test]
    fn the_payers_keypair_is_handed_out_off_a_live_cluster_only() {
        let live = live_adapter();
        let error = live.payer().unwrap_err();
        println!("{error:#}");
        assert_eq!(
            error.to_string(),
            "the payer's keypair is not handed out on a live cluster"
        );
        let local = local_adapter();
        let payer = local
            .payer()
            .expect("a runtime of the harness's own hands out the payer");
        assert_eq!(payer.pubkey(), local.payer_pubkey());
        assert_eq!(live.payer_pubkey(), live.payer.pubkey());
    }

    #[test]
    fn an_empty_discriminator_is_refused_on_any_runtime() {
        let program = Pubkey::new_from_array([9; 32]);
        for mut adapter in [live_adapter(), local_adapter()] {
            let error = adapter.allow_on_live_cluster(program, &[]).unwrap_err();
            assert!(
                error.to_string().contains("would allow every instruction"),
                "{error:#}"
            );
            adapter
                .allow_on_live_cluster(program, &[1; 8])
                .expect("a discriminator registers");
        }
    }
}
