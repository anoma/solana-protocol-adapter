use std::sync::Arc;

use anoma_pa_testkit::environment::{Environment as CoreEnvironment, Prover, State, StateBuilder};
use anoma_pa_testkit::transaction::Transaction;
use anyhow::Context;
use solana_instruction::Instruction;
use solana_keypair::Keypair;
use solana_signature::Signature;
use surfpool_sdk::{Pubkey, Surfnet};

use super::protocol_adapter::ProtocolAdapter;
use super::runtime;
use crate::state::actors::insert_default_signer;
use crate::state::cluster::{Cluster, insert_cluster};
use crate::state::pa::insert_pa_program;

/// Integration test execution environment: a surfpool runtime with the
/// adapter set up, and a prover. The `local` and `e2e` environments are this
/// with pa-testkit's local and queue provers.
///
/// Setup contract:
/// - All fields on this environment and its nested structures are public on purpose.
/// - Setup code should mutate/inspect the concrete environment directly.
/// - Test execution code should accept `impl anoma_pa_testkit::environment::Environment`
///   and use typed state helpers instead of concrete fields.
pub struct Environment<P> {
    pub surfnet: Surfnet,
    pub state: State,
    pub prover: P,
    pub protocol_adapter: ProtocolAdapter,
}

impl<P: Prover<Transaction = Transaction>> CoreEnvironment for Environment<P> {
    type Transaction = Transaction;
    type ProtocolAdapter = ProtocolAdapter;
    type Prover = P;

    fn prover(&self) -> &Self::Prover {
        &self.prover
    }

    fn state(&self) -> &State {
        &self.state
    }

    fn state_mut(&mut self) -> &mut State {
        &mut self.state
    }

    fn protocol_adapter(&self) -> &Self::ProtocolAdapter {
        &self.protocol_adapter
    }

    fn protocol_adapter_mut(&mut self) -> &mut Self::ProtocolAdapter {
        &mut self.protocol_adapter
    }
}

impl<P> Environment<P> {
    /// Deploys `so` at `program`, upgradeable, with `upgrade_authority` as its
    /// upgrade authority: a consumer's own program, such as a forwarder.
    pub fn deploy_program(
        &self,
        program: Pubkey,
        so: &[u8],
        upgrade_authority: Pubkey,
    ) -> anyhow::Result<()> {
        runtime::deploy(&self.surfnet, program, so, upgrade_authority)
    }

    /// Sends `instructions` as one transaction the default signer pays for,
    /// signed by it and `signers`, and waits for it to be confirmed: a
    /// consumer's setup, such as minting a token and approving a delegate.
    /// Returns its signature.
    pub async fn send(
        &self,
        instructions: &[Instruction],
        signers: &[&Keypair],
    ) -> anyhow::Result<Signature> {
        let adapter = &self.protocol_adapter;
        runtime::send_signed(&adapter.rpc, &adapter.payer, signers, instructions, &[]).await
    }

    /// The environment on `surfnet`, whose adapter `pa` is initialized: the
    /// protocol adapter read from it, and the test state.
    pub(in crate::envs) async fn assemble<F>(
        surfnet: Surfnet,
        cluster: Cluster,
        payer: Arc<Keypair>,
        pa: Pubkey,
        prover: P,
        insert_additional: F,
    ) -> anyhow::Result<Self>
    where
        F: AsyncFnOnce(&mut StateBuilder) -> anyhow::Result<()>,
    {
        let protocol_adapter =
            ProtocolAdapter::new(runtime::client(&surfnet), payer.clone(), pa).await?;
        let state = {
            let mut builder = StateBuilder::new();
            insert_cluster(&mut builder, cluster, surfnet.rpc_url().to_string());
            insert_default_signer(&mut builder, payer);
            insert_pa_program(&mut builder, pa);
            insert_additional(&mut builder)
                .await
                .context("failed to insert additional data into state")?;
            builder.finalize()
        };
        Ok(Self {
            surfnet,
            state,
            prover,
            protocol_adapter,
        })
    }
}
