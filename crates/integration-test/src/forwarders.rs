//! The forwarders a protocol adapter's settlements call. The proof commits
//! each external call's program, instruction data, expected output and
//! account count, but not the accounts: those are the submitter's to supply,
//! so a test registers, for each forwarder program its transactions call, how
//! a submitter passes it its calls' accounts. A forwarder is asked once per
//! settlement, for all its calls: what precedes the settlement can depend on
//! all of them, as when each of a forwarder's calls names the absolute index
//! of an instruction it needs.

use std::collections::HashMap;
use std::sync::Arc;

use anoma_pa_solana_client::external_call::SolanaExternalCall;
use anyhow::Context;
use futures::future::BoxFuture;
use solana_instruction::{AccountMeta, Instruction};
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use surfpool_sdk::Pubkey;

/// What a settlement passes a forwarder for its calls.
pub struct CallAccounts {
    /// Each call's CPI segment, in call order: the forwarder program, then
    /// the accounts its instruction takes.
    pub segments: Vec<Vec<AccountMeta>>,
    /// Instructions the settlement transaction carries before the settlement
    /// (an ed25519 signature check, an account a call needs), in the order the
    /// calls' instruction data names them.
    pub preceding: Vec<Instruction>,
}

/// How a submitter supplies a forwarder's calls with their accounts.
pub trait Forwarder: Send + Sync {
    /// The accounts a settlement passes this forwarder for `calls`, all of
    /// the settlement's calls to it in call order, as the chain behind `rpc`
    /// stands.
    fn call_accounts<'a>(
        &'a self,
        rpc: &'a RpcClient,
        calls: &'a [SolanaExternalCall],
    ) -> BoxFuture<'a, anyhow::Result<CallAccounts>>;
}

/// The forwarders registered with a protocol adapter, by program.
#[derive(Default)]
pub struct Forwarders(HashMap<Pubkey, Arc<dyn Forwarder>>);

impl Forwarders {
    /// Registers `forwarder` as the one that supplies `program`'s calls.
    pub fn register(&mut self, program: Pubkey, forwarder: Arc<dyn Forwarder>) {
        self.0.insert(program, forwarder);
    }

    /// The accounts of `calls`, in the order the adapter runs them: the
    /// instructions that precede the settlement, each forwarder's in the
    /// order of its first call, and each call's CPI segment. Each forwarder
    /// is asked once, for all its calls.
    pub async fn accounts(
        &self,
        rpc: &RpcClient,
        calls: &[SolanaExternalCall],
    ) -> anyhow::Result<(Vec<Instruction>, Vec<Vec<AccountMeta>>)> {
        // Each forwarder's calls by index, the forwarders in the order of
        // their first call.
        let mut by_forwarder: Vec<(Pubkey, Vec<usize>)> = Vec::new();
        for (i, call) in calls.iter().enumerate() {
            let program = Pubkey::new_from_array(call.program_id);
            match by_forwarder.iter_mut().find(|(p, _)| *p == program) {
                Some((_, indices)) => indices.push(i),
                None => by_forwarder.push((program, vec![i])),
            }
        }

        let mut preceding = Vec::new();
        let mut segments: Vec<Option<Vec<AccountMeta>>> = vec![None; calls.len()];
        for (program, indices) in by_forwarder {
            let forwarder = self.0.get(&program).with_context(|| {
                format!(
                    "external call {} is to {program}, for which no forwarder is registered",
                    indices[0]
                )
            })?;
            let its_calls: Vec<SolanaExternalCall> =
                indices.iter().map(|&i| calls[i].clone()).collect();
            let accounts = forwarder
                .call_accounts(rpc, &its_calls)
                .await
                .with_context(|| {
                    format!("the accounts of external calls {indices:?} to {program}")
                })?;
            anyhow::ensure!(
                accounts.segments.len() == indices.len(),
                "the forwarder {program} gives {} segments for its {} calls",
                accounts.segments.len(),
                indices.len()
            );
            for (&i, segment) in indices.iter().zip(accounts.segments) {
                anyhow::ensure!(
                    segment.len() == usize::from(calls[i].num_accounts),
                    "the forwarder {program} gives external call {i} {} accounts; the proof \
                     commits {}",
                    segment.len(),
                    calls[i].num_accounts
                );
                let first = segment.first().with_context(|| {
                    format!(
                        "the segment of external call {i} is empty, not led by the forwarder \
                         {program}"
                    )
                })?;
                anyhow::ensure!(
                    first.pubkey == program,
                    "the segment of external call {i} starts with {}, not the forwarder {program}",
                    first.pubkey
                );
                segments[i] = Some(segment);
            }
            preceding.extend(accounts.preceding);
        }
        let segments = segments
            .into_iter()
            .map(|segment| segment.expect("each call's forwarder gave its segment"))
            .collect();
        Ok((preceding, segments))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anoma_pa_solana_client::external_call::OutputMode;

    /// A forwarder whose calls take the forwarder and `extra` accounts, and
    /// one preceding instruction per call naming its first instruction byte;
    /// it counts the times it is asked.
    struct Fixed {
        extra: Vec<Pubkey>,
        asked: std::sync::atomic::AtomicUsize,
    }

    impl Fixed {
        fn new(extra: Vec<Pubkey>) -> Arc<Self> {
            Arc::new(Fixed {
                extra,
                asked: 0.into(),
            })
        }
    }

    impl Forwarder for Fixed {
        fn call_accounts<'a>(
            &'a self,
            _rpc: &'a RpcClient,
            calls: &'a [SolanaExternalCall],
        ) -> BoxFuture<'a, anyhow::Result<CallAccounts>> {
            Box::pin(async move {
                self.asked.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let mut accounts = CallAccounts {
                    segments: vec![],
                    preceding: vec![],
                };
                for call in calls {
                    let program = Pubkey::new_from_array(call.program_id);
                    let mut segment = vec![AccountMeta::new_readonly(program, false)];
                    segment.extend(self.extra.iter().map(|k| AccountMeta::new(*k, false)));
                    accounts.segments.push(segment);
                    accounts.preceding.push(Instruction::new_with_bytes(
                        program,
                        &call.instruction_data[..1],
                        vec![],
                    ));
                }
                Ok(accounts)
            })
        }
    }

    /// A forwarder whose calls take no accounts at all.
    struct Empty;

    impl Forwarder for Empty {
        fn call_accounts<'a>(
            &'a self,
            _rpc: &'a RpcClient,
            calls: &'a [SolanaExternalCall],
        ) -> BoxFuture<'a, anyhow::Result<CallAccounts>> {
            Box::pin(async move {
                Ok(CallAccounts {
                    segments: vec![vec![]; calls.len()],
                    preceding: vec![],
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
    async fn each_forwarder_is_asked_once_for_all_its_calls() {
        let rpc = RpcClient::new("http://127.0.0.1:1".to_string());
        let (a, b, x) = (
            Pubkey::new_unique(),
            Pubkey::new_unique(),
            Pubkey::new_unique(),
        );
        let (fixed_a, fixed_b) = (Fixed::new(vec![x]), Fixed::new(vec![]));
        let mut forwarders = Forwarders::default();
        forwarders.register(a, fixed_a.clone());
        forwarders.register(b, fixed_b.clone());

        let (preceding, segments) = forwarders
            .accounts(&rpc, &[call(a, 7, 2), call(b, 8, 1), call(a, 9, 2)])
            .await
            .unwrap();
        let asked = |f: &Fixed| f.asked.load(std::sync::atomic::Ordering::SeqCst);
        assert_eq!((asked(&fixed_a), asked(&fixed_b)), (1, 1));
        // Each forwarder's preceding instructions, in the order of its first
        // call; the segments in call order.
        assert_eq!(
            preceding.iter().map(|ix| ix.data[0]).collect::<Vec<_>>(),
            [7, 9, 8]
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

    /// A call whose proof commits no accounts, given an empty segment, is
    /// refused: a segment starts with the forwarder program.
    #[tokio::test]
    async fn an_empty_segment_is_refused() {
        let rpc = RpcClient::new("http://127.0.0.1:1".to_string());
        let program = Pubkey::new_unique();
        let mut forwarders = Forwarders::default();
        forwarders.register(program, Arc::new(Empty));
        assert_eq!(
            forwarders
                .accounts(&rpc, &[call(program, 7, 0)])
                .await
                .unwrap_err()
                .to_string(),
            format!("the segment of external call 0 is empty, not led by the forwarder {program}")
        );
    }
}
