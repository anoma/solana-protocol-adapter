use std::sync::Arc;

use anoma_pa_solana_client::settlement_input::{settled_resources, settlement_input};
use anoma_pa_solana_client::{
    PADDING_LEAF, PAStateAccount, SettlementRequest, TXDATA_EXPIRY_SLOTS_DEFAULT, decode_pa_state,
    derive_pa_state_pda, derive_root_marker_pda, plan_settlement,
};
use anoma_pa_testkit::commitment_tree::{FrontierCommitmentTree, depth_at};
use anoma_pa_testkit::environment::CommitmentTree as _;
use anoma_pa_testkit::environment::ProtocolAdapter as CoreProtocolAdapter;
use anoma_pa_testkit::environment::Transaction as CoreTransaction;
use anoma_pa_testkit::transaction::Transaction;
use anoma_rm_risc0::Digest;
use anoma_rm_risc0::transaction::Transaction as ArmTxn;
use anyhow::Context;
use solana_keypair::Keypair;
use solana_message::AddressLookupTableAccount;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_signer::Signer;
use surfpool_sdk::Pubkey;

use super::runtime::send;

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
    /// The next transaction-data upload's id; each settlement takes one.
    next_upload_id: u64,
}

impl ProtocolAdapter {
    /// The adapter `program`, with the commitment tree its state holds.
    pub(in crate::envs) async fn new(
        rpc: Arc<RpcClient>,
        payer: Arc<Keypair>,
        program: Pubkey,
        verifier_program: Pubkey,
        lookup_table: AddressLookupTableAccount,
    ) -> anyhow::Result<Self> {
        let state = read_state(&rpc, &program).await?;
        let count =
            usize::try_from(state.next_index).context("the commitment count exceeds usize")?;
        let sides = state
            .frontier
            .get(..depth_at(count))
            .with_context(|| {
                format!(
                    "the adapter stores {} frontier nodes at {count} leaves",
                    state.frontier.len()
                )
            })?
            .iter()
            .map(|side| Digest::from_bytes(*side))
            .collect();
        let adapter = Self {
            rpc,
            payer,
            program,
            verifier_program,
            lookup_table,
            commitment_tree: FrontierCommitmentTree::new(count, sides)?,
            next_upload_id: 0,
        };
        adapter.ensure_latest_root(&state)?;
        Ok(adapter)
    }

    /// The adapter's state account, as it is now.
    pub async fn state(&self) -> anyhow::Result<PAStateAccount> {
        read_state(&self.rpc, &self.program).await
    }

    /// Checks that the tree gives the root the adapter stores.
    fn ensure_latest_root(&self, state: &PAStateAccount) -> anyhow::Result<()> {
        let root = self.commitment_tree.root()?;
        anyhow::ensure!(
            root.as_bytes() == state.root,
            "the tree gives the root {root}, the adapter stores {}",
            Digest::from_bytes(state.root)
        );
        Ok(())
    }

    /// Checks that the adapter accepts every consumed resource's root: its
    /// current root, the padding leaf, or a root whose marker it holds.
    async fn assert_root_consistency(
        &self,
        tx: &ArmTxn,
        state: &PAStateAccount,
    ) -> anyhow::Result<()> {
        let (pa_state, _) = derive_pa_state_pda(&self.program);
        let aggregation = tx
            .aggregation
            .as_ref()
            .context("the transaction must be aggregated")?;
        for (action_idx, action) in aggregation.instance.actions.iter().enumerate() {
            for (resource_idx, consumed) in action.consumed_publics.iter().enumerate() {
                let root: [u8; 32] = consumed.commitment_tree_root.into();
                if root == state.root || root == PADDING_LEAF {
                    continue;
                }
                let (marker, _) = derive_root_marker_pda(&self.program, &pa_state, &root);
                let held = self
                    .rpc
                    .get_account_with_commitment(&marker, self.rpc.commitment())
                    .await
                    .with_context(|| {
                        format!(
                            "failed to query the root marker for action {action_idx} consumed \
                             resource {resource_idx}"
                        )
                    })?
                    .value
                    .is_some_and(|account| account.owner == self.program);
                anyhow::ensure!(
                    held,
                    "consumed commitment tree root not found in PA for action {action_idx} \
                     consumed resource {resource_idx}: root={}, pa_latest={}",
                    consumed.commitment_tree_root,
                    Digest::from_bytes(state.root)
                );
            }
        }
        Ok(())
    }
}

impl CoreProtocolAdapter for ProtocolAdapter {
    type Transaction = Transaction;
    type CommitmentTree = FrontierCommitmentTree;

    async fn execute(&mut self, transaction: Self::Transaction) -> anyhow::Result<()> {
        let created_commitments: Vec<Digest> = transaction.created_commitments()?.collect();
        let tx = transaction.into_arm();

        let state = self.state().await?;
        self.ensure_latest_root(&state)?;
        self.assert_root_consistency(&tx, &state).await?;

        let resources = settled_resources(&tx)?;
        let input = settlement_input(tx)?;
        let upload_id = self.next_upload_id;
        self.next_upload_id += 1;
        let expires_slot = self
            .rpc
            .get_slot()
            .await
            .context("failed to fetch the slot")?
            + TXDATA_EXPIRY_SLOTS_DEFAULT;
        let plan = plan_settlement(SettlementRequest {
            pa_program: self.program,
            payer: self.payer.pubkey(),
            upload_id,
            expires_slot,
            input: &input,
            state: &state,
            verifier_program: self.verifier_program,
            nullifiers: &resources.nullifiers,
            consumed_roots: &resources.consumed_roots,
            created: &resources.created,
            call_segments: Vec::new(),
        })?;

        let (rpc, payer) = (&*self.rpc, &*self.payer);
        send(rpc, payer, &[plan.init], &[])
            .await
            .context("failed to create the transaction-data upload")?;
        let settled = async {
            for (i, write) in plan.writes.into_iter().enumerate() {
                send(rpc, payer, &[write], &[])
                    .await
                    .with_context(|| format!("failed to write chunk {i} of the upload"))?;
            }
            send(
                rpc,
                payer,
                &plan.settle,
                std::slice::from_ref(&self.lookup_table),
            )
            .await
            .context("protocol adapter settlement failed")
        }
        .await;
        // The upload is closed whatever the settlement's outcome, so a refused
        // settlement does not strand its rent.
        send(rpc, payer, &[plan.close], &[])
            .await
            .context("failed to close the transaction-data upload")?;
        settled?;

        self.commitment_tree.add(created_commitments);
        self.ensure_latest_root(&self.state().await?)
    }

    fn commitment_tree(&self) -> &Self::CommitmentTree {
        &self.commitment_tree
    }
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
