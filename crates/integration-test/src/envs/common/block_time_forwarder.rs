//! The suite's block-time calls (`ExternalCall::BlockTime`): calls to the
//! adapter repository's example block-time forwarder
//! (`programs/block-time-forwarder`), deployed at its local address.

use std::sync::Arc;

use anoma_pa_solana_client::external_call::{OutputMode, SolanaExternalCall};
use anoma_pa_testkit::environment::TimeComparison;
use anoma_rm_risc0::utils::bytes_to_words;
use futures::future::BoxFuture;
use solana_instruction::AccountMeta;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;

use super::environment::Environment;
use crate::forwarders::{CallAccounts, Forwarder};

/// The forwarder's build, at the local address `env/localnet.env` names
/// `BLOCK_TIME_FORWARDER` (`dev.sh harness-programs`).
const BLOCK_TIME_FORWARDER_SO: &[u8] = include_bytes!("../../../programs/block_time_forwarder.so");

/// The accounts of a call to the block-time forwarder: the forwarder, then
/// the clock sysvar it reads the time from.
const NUM_ACCOUNTS: u8 = 2;

struct ClockReader;

impl Forwarder for ClockReader {
    fn call_accounts<'a>(
        &'a self,
        _rpc: &'a RpcClient,
        call: &'a SolanaExternalCall,
    ) -> BoxFuture<'a, anyhow::Result<CallAccounts>> {
        Box::pin(async move {
            Ok(CallAccounts {
                segment: vec![
                    AccountMeta::new_readonly(call.program_id.into(), false),
                    AccountMeta::new_readonly(solana_sdk_ids::sysvar::clock::ID, false),
                ],
                preceding: vec![],
            })
        })
    }
}

/// The external payload blob of a call to the block-time forwarder asking how
/// `time` compares with the clock's, expecting `expected`, after deploying
/// the forwarder in `env` and registering its accounts.
pub(in crate::envs) fn call<P>(
    env: &mut Environment<P>,
    time: u32,
    expected: TimeComparison,
) -> anyhow::Result<Vec<u32>> {
    let program = env.deploy_local_program("BLOCK_TIME_FORWARDER", BLOCK_TIME_FORWARDER_SO)?;
    env.protocol_adapter
        .forwarders
        .register(program, Arc::new(ClockReader));
    // The forwarder reads the time as an i64, little-endian.
    let call = SolanaExternalCall {
        program_id: program.to_bytes(),
        instruction_data: i64::from(time).to_le_bytes().to_vec(),
        expected_output: vec![expected as u8],
        output_mode: OutputMode::ReturnData,
        num_accounts: NUM_ACCOUNTS,
    };
    Ok(bytes_to_words(&call.encode()))
}
