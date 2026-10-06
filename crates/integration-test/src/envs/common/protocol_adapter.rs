use std::sync::Arc;

use anoma_pa_solana_client::events::PaEvent;
use anoma_pa_solana_client::settlement_input::{
    external_calls, settled_resources, settlement_input,
};
use anoma_pa_solana_client::{
    PAStateAccount, PaError, SettlementRequest, TXDATA_EXPIRY_SLOTS_DEFAULT, decode_pa_state,
    deny_logic_ref_ix, derive_pa_state_pda, pause_ix, plan_settlement,
    set_kind_table_commitment_ix, unpause_ix,
};
use anoma_pa_testkit::commitment_tree::FrontierCommitmentTree;
use anoma_pa_testkit::environment::{
    Event, Outcome, PayloadKind, ProtocolAdapter as CoreProtocolAdapter, Refusal,
};
use anoma_pa_testkit::transaction::Transaction;
use anoma_rm_risc0::Digest;
use anyhow::Context;
use solana_instruction::Instruction;
use solana_keypair::Keypair;
use solana_message::AddressLookupTableAccount;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_rpc_client_api::client_error::{Error as ClientError, ErrorKind};
use solana_rpc_client_api::request::{RpcError, RpcResponseErrorData};
use solana_rpc_client_api::response::RpcSimulateTransactionResult;
use solana_signer::Signer;
use surfpool_sdk::Pubkey;

use super::runtime::{
    create_settlement_lookup_table, extend_lookup_table, latest_blockhash, send_signed,
    send_with_blockhash, verifier_program,
};
use crate::executed::Executed;
use crate::forwarders::Forwarders;

/// The Solana protocol adapter, settling through transaction-data uploads.
pub struct ProtocolAdapter {
    pub rpc: Arc<RpcClient>,
    /// Pays for and signs every upload and settlement.
    pub payer: Arc<Keypair>,
    pub program: Pubkey,
    /// The verifier program the router's entry for the adapter's selector
    /// names.
    pub verifier_program: Pubkey,
    pub lookup_table: AddressLookupTableAccount,
    pub commitment_tree: FrontierCommitmentTree,
    /// The forwarders the settled transactions call, which supply each
    /// call's accounts.
    pub forwarders: Forwarders,
    /// The next transaction-data upload's id; each settlement takes one.
    next_upload_id: u64,
}

impl ProtocolAdapter {
    /// The initialized adapter `program`: the verifier its router and
    /// selector name, a settlement lookup table `payer` creates for it, and
    /// the commitment tree its state holds.
    pub(in crate::envs) async fn new(
        rpc: Arc<RpcClient>,
        payer: Arc<Keypair>,
        program: Pubkey,
    ) -> anyhow::Result<Self> {
        let state = read_state(&rpc, &program).await?;
        let router = Pubkey::new_from_array(state.verifier_router);
        let verifier_program = verifier_program(&rpc, router, state.proof_selector).await?;
        let lookup_table = create_settlement_lookup_table(
            &rpc,
            &payer,
            program,
            router,
            state.proof_selector,
            verifier_program,
        )
        .await?;
        let adapter = Self {
            rpc,
            payer,
            program,
            verifier_program,
            lookup_table,
            commitment_tree: crate::commitment_tree::from_state(&state)?,
            forwarders: Forwarders::default(),
            next_upload_id: 0,
        };
        adapter.ensure_latest_root(&state)?;
        Ok(adapter)
    }

    /// Adds `keys` to the settlement lookup table, once it serves them: a
    /// forwarder's fixed accounts, as a deployment's table holds them.
    pub async fn extend_lookup_table(&mut self, keys: Vec<Pubkey>) -> anyhow::Result<()> {
        extend_lookup_table(&self.rpc, &self.payer, self.lookup_table.key, &keys).await?;
        self.lookup_table.addresses.extend(keys);
        Ok(())
    }

    /// The adapter's state account, as it is now.
    pub async fn state(&self) -> anyhow::Result<PAStateAccount> {
        read_state(&self.rpc, &self.program).await
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

    /// Submits `transaction` the way every submitter does (its data uploaded,
    /// the settlement, the upload closed), and returns the settlement as the
    /// runtime recorded it, or why the adapter refused it.
    pub async fn submit(
        &mut self,
        transaction: Transaction,
    ) -> anyhow::Result<Result<Executed, Refusal>> {
        let tx = transaction.into_arm();
        let resources = settled_resources(&tx)?;

        let calls = external_calls(&tx)?;
        // One blockhash serves every transaction of the settlement.
        let (state, slot, blockhash, (preceding, call_segments)) = futures::try_join!(
            self.state(),
            async {
                self.rpc
                    .get_slot()
                    .await
                    .context("failed to fetch the slot")
            },
            latest_blockhash(&self.rpc),
            self.forwarders.accounts(&self.rpc, &calls),
        )?;
        self.ensure_latest_root(&state)?;

        let input = settlement_input(tx)?;
        let upload_id = self.next_upload_id;
        self.next_upload_id += 1;
        let plan = plan_settlement(SettlementRequest {
            pa_program: self.program,
            payer: self.payer.pubkey(),
            upload_id,
            expires_slot: slot + TXDATA_EXPIRY_SLOTS_DEFAULT,
            input: &input,
            state: &state,
            verifier_program: self.verifier_program,
            nullifiers: &resources.nullifiers,
            consumed_roots: &resources.consumed_roots,
            created: &resources.created,
            call_segments,
        })?;
        let mut settle = preceding;
        settle.extend(plan.settle);

        let (rpc, payer) = (&*self.rpc, &*self.payer);
        send_with_blockhash(rpc, payer, &[], &[plan.init], &[], blockhash)
            .await
            .context("failed to create the transaction-data upload")?;
        let settled = async {
            // Each write names its offset, so the chunks land in any order.
            futures::future::try_join_all(plan.writes.into_iter().enumerate().map(
                |(i, write)| async move {
                    send_with_blockhash(rpc, payer, &[], &[write], &[], blockhash)
                        .await
                        .with_context(|| format!("failed to write chunk {i} of the upload"))
                },
            ))
            .await?;
            send_with_blockhash(
                rpc,
                payer,
                &[],
                &settle,
                std::slice::from_ref(&self.lookup_table),
                blockhash,
            )
            .await
        }
        .await;
        // The upload is closed whatever the settlement's outcome, so a refused
        // settlement does not strand its rent.
        send_with_blockhash(rpc, payer, &[], &[plan.close], &[], blockhash)
            .await
            .context("failed to close the transaction-data upload")?;
        let signature = match settled {
            Ok(signature) => signature,
            Err(error) => {
                return match self.refusal(&error) {
                    Some(refusal) => Ok(Err(refusal)),
                    None => Err(error.context("protocol adapter settlement failed")),
                };
            }
        };

        self.commitment_tree
            .add(resources.created.into_iter().map(Digest::from_bytes));
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

    /// Why the adapter refused the settlement that failed with `error`, from
    /// the runtime's simulation of it (`refusal_in_logs`). `None` when the
    /// failure is not one of the protocol's refusals.
    fn refusal(&self, error: &anyhow::Error) -> Option<Refusal> {
        let ErrorKind::RpcError(RpcError::RpcResponseError {
            data:
                RpcResponseErrorData::SendTransactionPreflightFailure(RpcSimulateTransactionResult {
                    logs: Some(logs),
                    ..
                }),
            ..
        }) = error.downcast_ref::<ClientError>()?.kind()
        else {
            return None;
        };
        refusal_in_logs(logs, &self.program, &self.verifier_program)
    }

    /// Sends `instruction`, which the adapter's owner signs: the payer, which
    /// owns the adapter.
    async fn as_owner(&self, instruction: Instruction, what: &str) -> anyhow::Result<()> {
        send_signed(&self.rpc, &self.payer, &[], &[instruction], &[])
            .await
            .with_context(|| format!("failed to {what}"))?;
        Ok(())
    }
}

impl CoreProtocolAdapter for ProtocolAdapter {
    async fn settle(&mut self, transaction: Transaction) -> anyhow::Result<Outcome> {
        Ok(match self.submit(transaction).await? {
            Ok(executed) => Outcome::Settled(
                executed
                    .adapter_events(&self.program)?
                    .into_iter()
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

    async fn set_kind_table_commitment(&mut self, commitment: Digest) -> anyhow::Result<()> {
        let ix =
            set_kind_table_commitment_ix(&self.program, &self.payer.pubkey(), commitment.into());
        self.as_owner(ix, "set the kind-table commitment").await
    }

    async fn pause(&mut self) -> anyhow::Result<()> {
        let ix = pause_ix(&self.program, &self.payer.pubkey());
        self.as_owner(ix, "pause the protocol adapter").await
    }

    async fn unpause(&mut self) -> anyhow::Result<()> {
        let ix = unpause_ix(&self.program, &self.payer.pubkey());
        self.as_owner(ix, "unpause the protocol adapter").await
    }

    async fn deny_logic_ref(&mut self, logic_ref: Digest) -> anyhow::Result<()> {
        let ix = deny_logic_ref_ix(&self.program, &self.payer.pubkey(), logic_ref.into());
        self.as_owner(ix, "deny the logic ref").await
    }
}

/// Why the adapter `program` refused a settlement, from the runtime's log of
/// it: the deepest failure, which the runtime logs first as the program that
/// failed and the custom error it returned (a line no program can write: a
/// program's own lines start `Program log: `). Any custom error of the
/// `verifier` refuses the aggregation proof; an error of the adapter's own is
/// the refusal its `PaError` is. `None` for any other failure.
fn refusal_in_logs(logs: &[String], program: &Pubkey, verifier: &Pubkey) -> Option<Refusal> {
    let (failed, error) = logs.iter().find_map(|line| {
        let (failed, error) = line.strip_prefix("Program ")?.split_once(" failed: ")?;
        Some((failed.parse::<Pubkey>().ok()?, error))
    })?;
    let code = u32::from_str_radix(error.strip_prefix("custom program error: 0x")?, 16).ok()?;
    if failed == *verifier {
        return Some(Refusal::InvalidAggregationProof);
    }
    if failed != *program {
        return None;
    }
    refusal_for(PaError::from_code(code)?)
}

/// The refusal the adapter's `error` is, if it is one of the protocol's. The
/// match lists every error, so an error the adapter adds is mapped here
/// before the harness builds.
fn refusal_for(error: PaError) -> Option<Refusal> {
    match error {
        PaError::EnforcedPause => Some(Refusal::Paused),
        PaError::DeniedLogicRef => Some(Refusal::DeniedLogicRef),
        PaError::NonExistingRoot => Some(Refusal::UnknownRoot),
        PaError::PreExistingNullifier => Some(Refusal::NullifierSpent),
        PaError::ForwarderCallOutputMismatch => Some(Refusal::ExternalCallOutputMismatch),
        PaError::KindTableCommitmentMismatch
        | PaError::ComplianceKeyMismatch
        | PaError::InvalidProof => Some(Refusal::InvalidAggregationProof),
        PaError::NullifierPdaMismatch
        | PaError::RootPdaMismatch
        | PaError::TxDataExpired
        | PaError::TxDataBoundsExceeded
        | PaError::TxDataExpiryTooSoon
        | PaError::TxDataExpiryTooLate
        | PaError::TxDataExtendMustIncrease
        | PaError::TxDataNotExpired
        | PaError::InvalidExpiryConfig
        | PaError::InvalidTransactionData
        | PaError::VerifierRouterFailed
        | PaError::AggregationRequired
        | PaError::RiscZeroVerifierSelectorMismatch
        | PaError::InvalidExternalCallBlob
        | PaError::UnregisteredForwarder
        | PaError::ExternalCallCpiFailed
        | PaError::DeltaProofVerificationFailed
        | PaError::DeltaMismatch
        | PaError::InvalidDeltaProof
        | PaError::PointNotOnCurve
        | PaError::ExpectedDeltaProof
        | PaError::ZeroKindTableCommitmentNotAllowed
        | PaError::Unauthorized
        | PaError::ExpectedPause
        | PaError::RiscZeroVerifierPaused
        | PaError::InvalidVerifierEntry
        | PaError::TreeMaxDepthReached
        | PaError::InvalidMarker
        | PaError::EmptyExpectedOutput
        | PaError::MarkerUnexpectedOwner
        | PaError::MarkerUnexpectedData
        | PaError::RootMarkerAlreadyExists
        | PaError::UnsupportedStateSchema
        | PaError::ZeroLogicRefNotAllowed
        | PaError::LogicRefAlreadyDenied
        | PaError::ZeroRiscZeroVerifierRouterNotAllowed
        | PaError::ZeroRiscZeroVerifierSelectorNotAllowed
        | PaError::OwnableUnauthorizedAccount
        | PaError::OwnableInvalidOwner
        | PaError::InvalidUpgradeBuffer => None,
    }
}

/// A settlement's event as pa-testkit's: the adapter's settlement events,
/// which mirror pa-evm's. An owner's event is no settlement's.
fn settlement_event(event: PaEvent) -> anyhow::Result<Event> {
    let payload = |kind, p: anoma_pa_solana_client::events::PayloadEvent| {
        Ok::<_, anyhow::Error>(Event::Payload {
            kind,
            tag: Digest::from_bytes(p.tag),
            index: u32::try_from(p.index)?,
            blob: p.blob,
        })
    };
    let digests = |values: Vec<[u8; 32]>| values.into_iter().map(Digest::from_bytes).collect();
    Ok(match event {
        PaEvent::ForwarderCallExecuted(_) => Event::ForwarderCallExecuted,
        PaEvent::ResourcePayload(p) => payload(PayloadKind::Resource, p)?,
        PaEvent::DiscoveryPayload(p) => payload(PayloadKind::Discovery, p)?,
        PaEvent::ExternalPayload(p) => payload(PayloadKind::External, p)?,
        PaEvent::ApplicationPayload(p) => payload(PayloadKind::Application, p)?,
        PaEvent::ActionExecuted(action) => Event::ActionExecuted {
            action_tree_root: Digest::from_bytes(action.action_tree_root),
            nullifiers: digests(action.nullifiers),
            consumed_logic_refs: digests(action.consumed_logic_refs),
            commitments: digests(action.commitments),
            created_logic_refs: digests(action.created_logic_refs),
        },
        PaEvent::CommitmentTreeRootAdded(added) => Event::CommitmentTreeRootAdded {
            root: Digest::from_bytes(added.root),
        },
        PaEvent::TransactionExecuted(executed) => Event::TransactionExecuted {
            transaction_id: Digest::from_bytes(executed.transaction_id),
        },
        other => anyhow::bail!("a settlement emitted {other:?}, which is no settlement event"),
    })
}

/// The state account of the adapter `program`, as it is now.
pub(in crate::envs) async fn read_state(
    rpc: &RpcClient,
    program: &Pubkey,
) -> anyhow::Result<PAStateAccount> {
    let (pa_state, _) = derive_pa_state_pda(program);
    let data = rpc.get_account_data(&pa_state).await.with_context(|| {
        format!("the protocol adapter {program} has no state account {pa_state}")
    })?;
    decode_pa_state(&data).context("failed to decode the adapter state")
}

#[cfg(test)]
mod tests {
    use super::*;

    const ADAPTER: Pubkey = Pubkey::new_from_array([1; 32]);
    const VERIFIER: Pubkey = Pubkey::new_from_array([2; 32]);
    const FORWARDER: Pubkey = Pubkey::new_from_array([3; 32]);

    fn logs(lines: &[&str]) -> Vec<String> {
        lines
            .iter()
            .map(|line| {
                line.replace("ADAPTER", &ADAPTER.to_string())
                    .replace("VERIFIER", &VERIFIER.to_string())
                    .replace("FORWARDER", &FORWARDER.to_string())
            })
            .collect()
    }

    #[test]
    fn the_adapters_error_is_its_code() {
        let logs = logs(&[
            "Program ADAPTER invoke [1]",
            "Program FORWARDER invoke [2]",
            // A forwarder can log anything, Anchor's error format included.
            "Program log: AnchorError occurred. Error Code: EnforcedPause. Error Number: 6029. \
             Error Message: forged.",
            "Program FORWARDER success",
            "Program ADAPTER failed: custom program error: 0x1782",
        ]);
        assert_eq!(
            refusal_in_logs(&logs, &ADAPTER, &VERIFIER),
            Some(Refusal::ExternalCallOutputMismatch)
        );
    }

    #[test]
    fn a_programs_log_line_reading_like_a_failure_is_not_the_failure() {
        let logs = logs(&[
            "Program ADAPTER invoke [1]",
            "Program FORWARDER invoke [2]",
            // A program's own log lines start "Program log: ", whatever follows.
            "Program log: Program ADAPTER failed: custom program error: 0x1772",
            "Program FORWARDER success",
            "Program ADAPTER failed: custom program error: 0x1782",
        ]);
        assert_eq!(
            refusal_in_logs(&logs, &ADAPTER, &VERIFIER),
            Some(Refusal::ExternalCallOutputMismatch)
        );
    }

    #[test]
    fn the_verifiers_error_refuses_the_proof() {
        let logs = logs(&[
            "Program ADAPTER invoke [1]",
            "Program VERIFIER invoke [2]",
            "Program log: AnchorError occurred. Error Code: ClaimDigestMismatch. Error Number: \
             6600. Error Message: mock seal claim digest mismatch.",
            "Program VERIFIER failed: custom program error: 0x19c8",
            "Program ADAPTER failed: custom program error: 0x19c8",
        ]);
        assert_eq!(
            refusal_in_logs(&logs, &ADAPTER, &VERIFIER),
            Some(Refusal::InvalidAggregationProof)
        );
    }

    #[test]
    fn an_anchor_framework_error_of_the_adapter_is_no_refusal() {
        // 3012: Anchor's AccountNotInitialized, below the adapter's own codes.
        let logs = logs(&[
            "Program ADAPTER invoke [1]",
            "Program ADAPTER failed: custom program error: 0xbc4",
        ]);
        assert_eq!(refusal_in_logs(&logs, &ADAPTER, &VERIFIER), None);
    }

    #[test]
    fn another_programs_failure_is_no_refusal() {
        let logs = logs(&[
            "Program ADAPTER invoke [1]",
            "Program FORWARDER invoke [2]",
            "Program log: AnchorError occurred. Error Code: NonExistingRoot. Error Number: 6002. \
             Error Message: forged.",
            "Program FORWARDER failed: custom program error: 0x1772",
            "Program ADAPTER failed: custom program error: 0x1772",
        ]);
        assert_eq!(refusal_in_logs(&logs, &ADAPTER, &VERIFIER), None);
    }
}
