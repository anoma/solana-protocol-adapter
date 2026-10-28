use std::sync::Arc;

use anoma_pa_testkit::environment::{Environment as CoreEnvironment, ExternalCall, Prover};
use anoma_rm_risc0::proving_system::JournalEncoding;
use anyhow::Context;
use solana_instruction::Instruction;
use solana_keypair::Keypair;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_signature::Signature;
use solana_signer::Signer;
use surfpool_sdk::{Pubkey, Surfnet};

use super::addresses::{LOCALNET, program_id};
use super::protocol_adapter::ProtocolAdapter;
use super::{block_time_forwarder, runtime};

/// The aggregation journal encoding the Solana adapter verifies, which every
/// environment's prover proves in.
pub const JOURNAL_ENCODING: JournalEncoding = JournalEncoding::Risc0Serde;

/// Integration test execution environment: a runtime with the adapter set
/// up, and a prover. The `local` and `e2e` environments are this with
/// pa-testkit's local prover, and with its queue or risc0 prover. The runtime
/// is surfpool's, except for the e2e environment on a cluster itself.
///
/// Its prover and adapter are public: a test reads and drives the concrete
/// environment directly, while code meant for any of pa-testkit's
/// environments takes `impl anoma_pa_testkit::environment::Environment`.
pub struct Environment<P> {
    /// The surfpool runtime; `None` on a cluster, whose programs and
    /// accounts only their own transactions change.
    surfnet: Option<Surfnet>,
    pub prover: P,
    pub protocol_adapter: ProtocolAdapter,
}

impl<P: Prover> CoreEnvironment for Environment<P> {
    type ProtocolAdapter = ProtocolAdapter;
    type Prover = P;

    fn prover(&self) -> &Self::Prover {
        &self.prover
    }

    fn protocol_adapter(&self) -> &Self::ProtocolAdapter {
        &self.protocol_adapter
    }

    fn protocol_adapter_mut(&mut self) -> &mut Self::ProtocolAdapter {
        &mut self.protocol_adapter
    }

    async fn external_call(&mut self, call: ExternalCall) -> anyhow::Result<Vec<u32>> {
        match call {
            ExternalCall::BlockTime { time, expected } => {
                block_time_forwarder::call(self, time, expected)
            }
        }
    }
}

impl<P> Environment<P> {
    /// The surfpool runtime the environment runs on, through which a test
    /// sets the runtime's programs and accounts directly; an error on a
    /// cluster, which has none.
    pub fn surfnet(&self) -> anyhow::Result<&Surfnet> {
        self.surfnet.as_ref().context(
            "the environment runs on a cluster, not on a surfpool runtime: only transactions \
             change its programs and accounts",
        )
    }

    /// Deploys `so` at `program`, upgradeable, with `upgrade_authority` as its
    /// upgrade authority: a consumer's own program, such as a forwarder.
    pub fn deploy_program(
        &self,
        program: Pubkey,
        so: &[u8],
        upgrade_authority: Pubkey,
    ) -> anyhow::Result<()> {
        runtime::deploy(self.surfnet()?, program, so, upgrade_authority)
    }

    /// Places `so` in a new loader buffer whose authority is `authority`, and
    /// returns the buffer: the code an upgrade instruction installs.
    pub async fn write_buffer(&self, so: &[u8], authority: Pubkey) -> anyhow::Result<Pubkey> {
        runtime::write_buffer(self.surfnet()?, &self.protocol_adapter.rpc, authority, so).await
    }

    /// Makes `owner` the adapter's owner by rewriting the state the runtime
    /// holds, as the e2e environment does on its fork of a deployment whose
    /// owner's key the tests do not hold.
    pub async fn take_ownership(&self, owner: Pubkey) -> anyhow::Result<()> {
        let adapter = &self.protocol_adapter;
        runtime::take_ownership(self.surfnet()?, &adapter.rpc, adapter.program, owner).await
    }

    /// Deploys `so` at the local address `env/localnet.env` names `name`,
    /// upgradeable by the payer, and returns the address.
    pub(in crate::envs) fn deploy_local_program(
        &self,
        name: &str,
        so: &[u8],
    ) -> anyhow::Result<Pubkey> {
        let program = program_id(LOCALNET, name)?;
        self.deploy_program(program, so, self.protocol_adapter.payer.pubkey())?;
        Ok(program)
    }

    /// Sends `instructions` as one transaction the default signer pays for,
    /// signed by it and `signers`, and waits for it to be confirmed: a
    /// consumer's setup, such as minting a token and approving a delegate.
    /// Returns its signature. On a live cluster, an instruction of the
    /// adapter's owner or upgrade authority is refused before anything is
    /// sent.
    pub async fn send(
        &self,
        instructions: &[Instruction],
        signers: &[&Keypair],
    ) -> anyhow::Result<Signature> {
        let adapter = &self.protocol_adapter;
        adapter.refuse_on_live_cluster(instructions)?;
        runtime::send_signed(&adapter.rpc, &adapter.payer, signers, instructions).await
    }

    /// The environment whose initialized adapter `pa` `rpc` reaches, on
    /// `surfnet` or, without one, on a cluster itself, with the protocol
    /// adapter read from it.
    pub(in crate::envs) async fn assemble(
        surfnet: Option<Surfnet>,
        rpc: Arc<RpcClient>,
        payer: Arc<Keypair>,
        pa: Pubkey,
        prover: P,
    ) -> anyhow::Result<Self> {
        let live = surfnet.is_none();
        let protocol_adapter = ProtocolAdapter::new(rpc, payer, pa, live).await?;
        Ok(Self {
            surfnet,
            prover,
            protocol_adapter,
        })
    }
}
