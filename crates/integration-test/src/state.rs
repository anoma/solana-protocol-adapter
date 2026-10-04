//! What an environment records in its test state for the code that drives it:
//! the runtime's RPC endpoint, the default signer, and the protocol adapter's
//! program address.

use std::sync::Arc;

use anoma_pa_testkit::environment::{Environment, State, StateBuilder};
use anyhow::Context;
use solana_keypair::Keypair;
use surfpool_sdk::Pubkey;

pub const KEY_RPC_URL: &str = "solana.rpc_url";
pub const KEY_DEFAULT_SIGNER: &str = "solana.actor.default_signer";
pub const KEY_PA_PROGRAM: &str = "solana.pa.program";

pub(crate) fn insert(
    builder: &mut StateBuilder,
    rpc_url: String,
    signer: Arc<Keypair>,
    pa: Pubkey,
) {
    builder.insert(KEY_RPC_URL, rpc_url);
    builder.insert(KEY_DEFAULT_SIGNER, signer);
    builder.insert(KEY_PA_PROGRAM, pa);
}

/// The runtime's RPC endpoint.
pub fn rpc_url<E: Environment>(env: &E) -> anyhow::Result<String> {
    rpc_url_in_state(env.state())
}

pub fn rpc_url_in_state(state: &State) -> anyhow::Result<String> {
    state
        .get::<String>(KEY_RPC_URL)
        .cloned()
        .context("failed to retrieve the RPC endpoint from env")
}

/// The funded keypair that pays for and signs the environment's transactions.
pub fn default_signer<E: Environment>(env: &E) -> anyhow::Result<Arc<Keypair>> {
    default_signer_in_state(env.state())
}

pub fn default_signer_in_state(state: &State) -> anyhow::Result<Arc<Keypair>> {
    state
        .get::<Arc<Keypair>>(KEY_DEFAULT_SIGNER)
        .cloned()
        .context("failed to retrieve default signer from env")
}

/// The protocol adapter's program address.
pub fn pa_program<E: Environment>(env: &E) -> anyhow::Result<Pubkey> {
    pa_program_in_state(env.state())
}

pub fn pa_program_in_state(state: &State) -> anyhow::Result<Pubkey> {
    state
        .get::<Pubkey>(KEY_PA_PROGRAM)
        .copied()
        .context("failed to retrieve protocol adapter program from env")
}
