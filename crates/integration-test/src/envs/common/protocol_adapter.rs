use std::collections::HashSet;
use std::sync::Arc;

use anoma_pa_solana_client::settlement_input::{
    external_calls, settled_resources, settlement_input,
};
use anoma_pa_solana_client::{
    PADDING_LEAF, PAStateAccount, SettlementRequest, TXDATA_EXPIRY_SLOTS_DEFAULT, decode_pa_state,
    derive_pa_state_pda, derive_root_marker_pda, plan_settlement,
};
use anoma_pa_testkit::commitment_tree::FrontierCommitmentTree;
use anoma_pa_testkit::environment::CommitmentTree as _;
use anoma_pa_testkit::environment::ProtocolAdapter as CoreProtocolAdapter;
use anoma_pa_testkit::transaction::Transaction;
use anoma_rm_risc0::Digest;
use anoma_rm_risc0::transaction::Transaction as ArmTxn;
use anyhow::Context;
use solana_keypair::Keypair;
use solana_message::AddressLookupTableAccount;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_signature::Signature;
use solana_signer::Signer;
use surfpool_sdk::Pubkey;

use super::runtime::{
    create_settlement_lookup_table, extend_lookup_table, latest_blockhash, send_with_blockhash,
    verifier_program,
};
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
        let root = self.commitment_tree.root()?;
        anyhow::ensure!(
            root.as_bytes() == state.root,
            "the tree gives the root {root}, the adapter stores {}",
            Digest::from_bytes(state.root)
        );
        Ok(())
    }

    /// Checks that the adapter accepts every consumed resource's root (the
    /// transaction `tx` consumes `consumed_roots`): its current root, the
    /// padding leaf, or a root whose marker it holds, the markers read in one
    /// request.
    async fn assert_root_consistency(
        &self,
        tx: &ArmTxn,
        consumed_roots: &[[u8; 32]],
        state: &PAStateAccount,
    ) -> anyhow::Result<()> {
        let mut seen = HashSet::new();
        let roots: Vec<[u8; 32]> = consumed_roots
            .iter()
            .copied()
            .filter(|root| *root != state.root && *root != PADDING_LEAF && seen.insert(*root))
            .collect();
        let (pa_state, _) = derive_pa_state_pda(&self.program);
        let markers: Vec<Pubkey> = roots
            .iter()
            .map(|root| derive_root_marker_pda(&self.program, &pa_state, root).0)
            .collect();
        let accounts = self
            .rpc
            .get_multiple_accounts(&markers)
            .await
            .context("failed to query the consumed roots' markers")?;
        let Some(missing) = roots.iter().zip(&accounts).find_map(|(root, account)| {
            (!account.as_ref().is_some_and(|a| a.owner == self.program)).then_some(*root)
        }) else {
            return Ok(());
        };
        let (action_idx, resource_idx) = tx
            .aggregation
            .as_ref()
            .context("the transaction must be aggregated")?
            .instance
            .actions
            .iter()
            .enumerate()
            .find_map(|(i, action)| {
                action
                    .consumed_publics
                    .iter()
                    .position(|c| <[u8; 32]>::from(c.commitment_tree_root) == missing)
                    .map(|j| (i, j))
            })
            .context("no consumed resource names the root the adapter does not hold")?;
        anyhow::bail!(
            "consumed commitment tree root not found in PA for action {action_idx} consumed \
             resource {resource_idx}: root={}, pa_latest={}",
            Digest::from_bytes(missing),
            Digest::from_bytes(state.root)
        )
    }

    /// Settles `transaction` the way every submitter does (its data uploaded,
    /// the settlement, the upload closed), and returns the settlement's
    /// signature, which a test reads the settlement's log and events with.
    pub async fn settle(&mut self, transaction: Transaction) -> anyhow::Result<Signature> {
        let tx = transaction.into_arm();
        let resources = settled_resources(&tx)?;

        let (state, slot) = futures::try_join!(self.state(), async {
            self.rpc
                .get_slot()
                .await
                .context("failed to fetch the slot")
        })?;
        self.ensure_latest_root(&state)?;
        self.assert_root_consistency(&tx, &resources.consumed_roots, &state)
            .await?;

        let (preceding, call_segments) = self
            .forwarders
            .accounts(&self.rpc, &external_calls(&tx)?)
            .await?;
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

        // One blockhash serves every transaction of the settlement.
        let (rpc, payer) = (&*self.rpc, &*self.payer);
        let blockhash = latest_blockhash(rpc).await?;
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
            .context("protocol adapter settlement failed")
        }
        .await;
        // The upload is closed whatever the settlement's outcome, so a refused
        // settlement does not strand its rent.
        send_with_blockhash(rpc, payer, &[], &[plan.close], &[], blockhash)
            .await
            .context("failed to close the transaction-data upload")?;
        let signature = settled?;

        self.commitment_tree
            .add(resources.created.into_iter().map(Digest::from_bytes));
        self.ensure_latest_root(&self.state().await?)?;
        Ok(signature)
    }
}

impl CoreProtocolAdapter for ProtocolAdapter {
    type Transaction = Transaction;
    type CommitmentTree = FrontierCommitmentTree;

    async fn execute(&mut self, transaction: Self::Transaction) -> anyhow::Result<()> {
        self.settle(transaction).await?;
        Ok(())
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
