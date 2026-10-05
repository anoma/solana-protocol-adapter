//! The forwarders a protocol adapter's settlements call. The proof commits
//! each external call's program, instruction data, expected output and
//! account count, but not the accounts: those are the submitter's to supply,
//! so a test registers, for each forwarder program its transactions call, how
//! a submitter passes it a call's accounts.

use std::collections::HashMap;
use std::sync::Arc;

use anoma_pa_solana_client::external_call::SolanaExternalCall;
use anyhow::Context;
use futures::future::BoxFuture;
use solana_instruction::{AccountMeta, Instruction};
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use surfpool_sdk::Pubkey;

/// What a settlement passes a forwarder for one call.
pub struct CallAccounts {
    /// The call's CPI segment: the forwarder program, then the accounts its
    /// instruction takes.
    pub segment: Vec<AccountMeta>,
    /// Instructions the settlement transaction carries before the settlement
    /// (an ed25519 signature check, an account the call needs), in the order
    /// the call's instruction data names them.
    pub preceding: Vec<Instruction>,
}

/// How a submitter supplies a forwarder's calls with their accounts.
pub trait Forwarder: Send + Sync {
    /// The accounts a settlement passes this forwarder for `call`, as the
    /// chain behind `rpc` stands.
    fn call_accounts<'a>(
        &'a self,
        rpc: &'a RpcClient,
        call: &'a SolanaExternalCall,
    ) -> BoxFuture<'a, anyhow::Result<CallAccounts>>;
}

/// The forwarders registered with a protocol adapter, by program.
#[derive(Clone, Default)]
pub struct Forwarders(HashMap<Pubkey, Arc<dyn Forwarder>>);

impl Forwarders {
    /// Registers `forwarder` as the one that supplies `program`'s calls.
    pub fn register(&mut self, program: Pubkey, forwarder: Arc<dyn Forwarder>) {
        self.0.insert(program, forwarder);
    }

    /// The accounts of `calls`, in the order the adapter runs them: the
    /// instructions that precede the settlement, every call's in call order,
    /// and each call's CPI segment.
    pub async fn accounts(
        &self,
        rpc: &RpcClient,
        calls: &[SolanaExternalCall],
    ) -> anyhow::Result<(Vec<Instruction>, Vec<Vec<AccountMeta>>)> {
        let mut preceding = Vec::new();
        let mut segments = Vec::with_capacity(calls.len());
        for (i, call) in calls.iter().enumerate() {
            let program = Pubkey::new_from_array(call.program_id);
            let forwarder = self.0.get(&program).with_context(|| {
                format!("external call {i} is to {program}, for which no forwarder is registered")
            })?;
            let accounts = forwarder
                .call_accounts(rpc, call)
                .await
                .with_context(|| format!("the accounts of external call {i} to {program}"))?;
            anyhow::ensure!(
                accounts.segment.len() == usize::from(call.num_accounts),
                "the forwarder {program} gives external call {i} {} accounts; the proof commits {}",
                accounts.segment.len(),
                call.num_accounts
            );
            anyhow::ensure!(
                accounts.segment[0].pubkey == program,
                "the segment of external call {i} starts with {}, not the forwarder {program}",
                accounts.segment[0].pubkey
            );
            preceding.extend(accounts.preceding);
            segments.push(accounts.segment);
        }
        Ok((preceding, segments))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anoma_pa_solana_client::external_call::OutputMode;

    /// A forwarder whose calls take the forwarder and `extra` accounts, and
    /// one preceding instruction naming the call's first instruction byte.
    struct Fixed {
        extra: Vec<Pubkey>,
    }

    impl Forwarder for Fixed {
        fn call_accounts<'a>(
            &'a self,
            _rpc: &'a RpcClient,
            call: &'a SolanaExternalCall,
        ) -> BoxFuture<'a, anyhow::Result<CallAccounts>> {
            Box::pin(async move {
                let program = Pubkey::new_from_array(call.program_id);
                let mut segment = vec![AccountMeta::new_readonly(program, false)];
                segment.extend(self.extra.iter().map(|k| AccountMeta::new(*k, false)));
                Ok(CallAccounts {
                    segment,
                    preceding: vec![Instruction::new_with_bytes(
                        program,
                        &call.instruction_data[..1],
                        vec![],
                    )],
                })
            })
        }
    }

    fn call(program: Pubkey, byte: u8, num_accounts: u8) -> SolanaExternalCall {
        SolanaExternalCall {
            program_id: program.to_bytes(),
            instruction_data: vec![byte],
            expected_output: vec![1],
            output_mode: OutputMode::ReturnData,
            num_accounts,
        }
    }

    #[tokio::test]
    async fn each_calls_accounts_come_from_its_programs_forwarder_in_call_order() {
        let rpc = RpcClient::new("http://127.0.0.1:1".to_string());
        let (a, b, x) = (
            Pubkey::new_unique(),
            Pubkey::new_unique(),
            Pubkey::new_unique(),
        );
        let mut forwarders = Forwarders::default();
        forwarders.register(a, Arc::new(Fixed { extra: vec![x] }));
        forwarders.register(b, Arc::new(Fixed { extra: vec![] }));

        let (preceding, segments) = forwarders
            .accounts(&rpc, &[call(a, 7, 2), call(b, 8, 1), call(a, 9, 2)])
            .await
            .unwrap();
        assert_eq!(
            preceding.iter().map(|ix| ix.data[0]).collect::<Vec<_>>(),
            [7, 8, 9]
        );
        assert_eq!(
            segments
                .iter()
                .map(|s| s.iter().map(|m| m.pubkey).collect::<Vec<_>>())
                .collect::<Vec<_>>(),
            vec![vec![a, x], vec![b], vec![a, x]]
        );

        let unregistered = Pubkey::new_unique();
        assert_eq!(
            forwarders
                .accounts(&rpc, &[call(a, 7, 2), call(unregistered, 1, 1)])
                .await
                .unwrap_err()
                .to_string(),
            format!("external call 1 is to {unregistered}, for which no forwarder is registered")
        );
        assert_eq!(
            forwarders
                .accounts(&rpc, &[call(a, 7, 3)])
                .await
                .unwrap_err()
                .to_string(),
            format!("the forwarder {a} gives external call 0 2 accounts; the proof commits 3")
        );
    }
}
