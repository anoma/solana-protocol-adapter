//! A confirmed transaction as a test reads it back: the transaction as sent,
//! the runtime's log of it, and the instructions its programs invoked, among them the events a program emits
//! by invoking itself (Anchor's `emit_cpi!`).

use anchor_lang::event::EVENT_IX_TAG_LE;
use anchor_lang::{AnchorDeserialize, Discriminator};
use anoma_pa_solana_client::decode_cpi_event;
use anyhow::Context;
use solana_commitment_config::CommitmentConfig;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_rpc_client_types::config::RpcTransactionConfig;
use solana_signature::Signature;
use solana_transaction::versioned::VersionedTransaction;
use solana_transaction_status_client_types::{
    UiInnerInstructions, UiInstruction, UiTransactionEncoding,
};
use surfpool_sdk::Pubkey;

/// What the runtime recorded of one confirmed transaction.
pub struct Executed {
    /// The transaction as it was sent.
    pub transaction: VersionedTransaction,
    /// The transaction's log, as the runtime kept it: past its byte limit
    /// the runtime truncates it.
    pub logs: Vec<String>,
    /// Every instruction the transaction's programs invoked: the invoked
    /// program and the instruction data, in the order they ran.
    pub inner: Vec<(Pubkey, Vec<u8>)>,
}

impl Executed {
    /// The confirmed transaction `signature`, read from `rpc`.
    pub async fn read(rpc: &RpcClient, signature: &Signature) -> anyhow::Result<Self> {
        let confirmed = rpc
            .get_transaction_with_config(
                signature,
                RpcTransactionConfig {
                    encoding: Some(UiTransactionEncoding::Base64),
                    commitment: Some(CommitmentConfig::confirmed()),
                    max_supported_transaction_version: Some(1),
                },
            )
            .await
            .with_context(|| format!("failed to read the transaction {signature}"))?
            .transaction;
        let meta = confirmed
            .meta
            .with_context(|| format!("the runtime keeps no status of {signature}"))?;
        let transaction = confirmed
            .transaction
            .decode()
            .with_context(|| format!("failed to decode the transaction {signature}"))?;

        // An instruction names its program by index into the message's keys,
        // all of them in the message in the transactions the harness sends.
        let keys = transaction.message.static_account_keys();
        let groups: Option<Vec<UiInnerInstructions>> = meta.inner_instructions.into();
        let mut inner = Vec::new();
        for group in groups.unwrap_or_default() {
            for instruction in group.instructions {
                let UiInstruction::Compiled(instruction) = instruction else {
                    anyhow::bail!(
                        "{signature} has a parsed inner instruction; base64 encodes none"
                    );
                };
                let program = *keys
                    .get(usize::from(instruction.program_id_index))
                    .with_context(|| {
                        format!(
                            "an inner instruction of {signature} names key {}, of {}",
                            instruction.program_id_index,
                            keys.len()
                        )
                    })?;
                let data = bs58::decode(&instruction.data)
                    .into_vec()
                    .with_context(|| {
                        format!("an inner instruction of {signature} has data that is not base58")
                    })?;
                inner.push((program, data));
            }
        }
        let logs: Option<Vec<String>> = meta.log_messages.into();
        Ok(Self {
            transaction,
            logs: logs.unwrap_or_default(),
            inner,
        })
    }

    /// The data of the transaction's first top-level instruction to
    /// `program`.
    pub fn instruction_data(&self, program: &Pubkey) -> Option<&[u8]> {
        let message = &self.transaction.message;
        let keys = message.static_account_keys();
        message
            .instructions()
            .iter()
            .find(|ix| keys.get(usize::from(ix.program_id_index)) == Some(program))
            .map(|ix| ix.data.as_slice())
    }

    /// The instruction data of every event `program` emitted, in order: its
    /// invocations of itself that carry Anchor's event tag.
    pub fn cpi_events<'a>(&'a self, program: &'a Pubkey) -> impl Iterator<Item = &'a [u8]> {
        self.inner
            .iter()
            .filter(move |(invoked, data)| invoked == program && data.starts_with(EVENT_IX_TAG_LE))
            .map(|(_, data)| data.as_slice())
    }

    /// The events of type `E` that `program` emitted, in order: its event
    /// self-invocations that carry an `E`, each decoded whole
    /// (`decode_cpi_event`); its other events are passed over.
    pub fn events<E: Discriminator + AnchorDeserialize>(
        &self,
        program: &Pubkey,
    ) -> anyhow::Result<Vec<E>> {
        self.cpi_events(program)
            .filter_map(|data| {
                decode_cpi_event::<E>(data)
                    .with_context(|| {
                        format!(
                            "the event {data:02x?} of {program} with the discriminator of {} does \
                             not decode as one",
                            std::any::type_name::<E>()
                        )
                    })
                    .transpose()
            })
            .collect()
    }
}

/// The instruction data of the self-invocation emitting `event`.
#[cfg(test)]
pub(crate) fn emitted<E: Discriminator + anchor_lang::AnchorSerialize>(event: &E) -> Vec<u8> {
    let mut data = [EVENT_IX_TAG_LE, E::DISCRIMINATOR].concat();
    event.serialize(&mut data).unwrap();
    data
}

#[cfg(test)]
mod tests {
    use super::*;
    use anoma_pa_solana_client::generated::protocol_adapter::events::{
        ActionExecuted, TransactionExecuted,
    };

    const ADAPTER: Pubkey = Pubkey::new_from_array([1; 32]);

    fn executed(inner: Vec<(Pubkey, Vec<u8>)>) -> Executed {
        Executed {
            transaction: VersionedTransaction::default(),
            logs: vec![],
            inner,
        }
    }

    fn transaction_executed(id: u8) -> Vec<u8> {
        emitted(&TransactionExecuted {
            transaction_id: [id; 32],
        })
    }

    #[test]
    fn a_programs_events_of_one_type_decode_in_order() {
        let action = ActionExecuted {
            action_tree_root: [1; 32],
            nullifiers: vec![[2; 32]],
            consumed_logic_refs: vec![[3; 32]],
            commitments: vec![[4; 32], [5; 32]],
            created_logic_refs: vec![[6; 32], [7; 32]],
        };
        let executed = executed(vec![
            (ADAPTER, transaction_executed(1)),
            (ADAPTER, emitted(&action)),
            // Another program's event, and an instruction of the adapter's
            // that is no event.
            (Pubkey::new_from_array([2; 32]), transaction_executed(3)),
            (ADAPTER, vec![0; 16]),
            (ADAPTER, transaction_executed(2)),
        ]);
        let ids: Vec<_> = executed
            .events::<TransactionExecuted>(&ADAPTER)
            .unwrap()
            .into_iter()
            .map(|event| event.transaction_id)
            .collect();
        assert_eq!(ids, [[1; 32], [2; 32]]);
        let actions = executed.events::<ActionExecuted>(&ADAPTER).unwrap();
        println!("{actions:?}");
        let [decoded] = &actions[..] else {
            panic!("{} ActionExecuted events, not one", actions.len());
        };
        assert_eq!(decoded.action_tree_root, [1; 32]);
        assert_eq!(decoded.nullifiers, [[2; 32]]);
        assert_eq!(decoded.consumed_logic_refs, [[3; 32]]);
        assert_eq!(decoded.commitments, [[4; 32], [5; 32]]);
        assert_eq!(decoded.created_logic_refs, [[6; 32], [7; 32]]);
    }

    #[test]
    fn an_event_whose_body_does_not_decode_is_an_error() {
        let mut data = transaction_executed(1);
        data.push(0);
        let error = executed(vec![(ADAPTER, data)])
            .events::<TransactionExecuted>(&ADAPTER)
            .unwrap_err();
        println!("{error:#}");
        assert!(
            format!("{error:#}").contains("bytes remain after the event body"),
            "{error:#}"
        );
    }
}
