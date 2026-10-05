use std::sync::Arc;

use anoma_pa_testkit::environment::{Environment as CoreEnvironment, Prover, State, StateBuilder};
use anoma_pa_testkit::transaction::Transaction;
use solana_instruction::Instruction;
use solana_keypair::Keypair;
use solana_signature::Signature;
use surfpool_sdk::{Pubkey, Surfnet};

use super::protocol_adapter::ProtocolAdapter;
use super::runtime;

/// Integration test execution environment: a surfpool runtime with the
/// adapter set up, and a prover. The `local` and `e2e` environments are this
/// with pa-testkit's local prover, and with its queue or risc0 prover.
///
/// Its fields are public: a test reads and drives the concrete environment
/// directly, while code meant for any of pa-testkit's environments takes
/// `impl anoma_pa_testkit::environment::Environment`.
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

    /// Places `so` in a new loader buffer whose authority is `authority`, and
    /// returns the buffer: the code an upgrade instruction installs.
    pub async fn write_buffer(&self, so: &[u8], authority: Pubkey) -> anyhow::Result<Pubkey> {
        runtime::write_buffer(&self.surfnet, &self.protocol_adapter.rpc, authority, so).await
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
    /// protocol adapter read from it, and an empty test state.
    pub(in crate::envs) async fn assemble(
        surfnet: Surfnet,
        payer: Arc<Keypair>,
        pa: Pubkey,
        prover: P,
    ) -> anyhow::Result<Self> {
        let protocol_adapter = ProtocolAdapter::new(runtime::client(&surfnet), payer, pa).await?;
        Ok(Self {
            surfnet,
            state: StateBuilder::new().finalize(),
            prover,
            protocol_adapter,
        })
    }
}
