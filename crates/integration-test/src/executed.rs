//! A confirmed transaction as a test reads it back: the transaction as sent,
//! the runtime's log of it, and the instructions its programs invoked, among them the events a program emits
//! by invoking itself (Anchor's `emit_cpi!`).

use anchor_lang::event::EVENT_IX_TAG_LE;
use anchor_lang::{AnchorDeserialize, Discriminator};
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

    /// The events the adapter `program` emitted, decoded, in order.
    pub fn adapter_events(&self, program: &Pubkey) -> anyhow::Result<Vec<PaEvent>> {
        self.cpi_events(program).map(decode_adapter_event).collect()
    }
}

macro_rules! adapter_events {
    ($($event:ident),* $(,)?) => {
        /// An event of the adapter, decoded with the program's own type.
        #[derive(Debug)]
        pub enum PaEvent {
            $($event(protocol_adapter::$event),)*
        }

        /// The event an adapter's event self-invocation carries: Anchor's
        /// event tag, then the event's discriminator, then its Borsh body,
        /// which ends the data.
        pub fn decode_adapter_event(data: &[u8]) -> anyhow::Result<PaEvent> {
            let event = data
                .strip_prefix(EVENT_IX_TAG_LE)
                .with_context(|| format!("{data:02x?} does not start with Anchor's event tag"))?;
            $(
                if let Some(body) = event.strip_prefix(protocol_adapter::$event::DISCRIMINATOR) {
                    return Ok(PaEvent::$event(event_body(body)?));
                }
            )*
            anyhow::bail!(
                "the adapter emits no event whose discriminator starts {:02x?}",
                &event[..event.len().min(8)]
            )
        }
    };
}

adapter_events!(
    ResourcePayload,
    DiscoveryPayload,
    ExternalPayload,
    ApplicationPayload,
    ActionExecuted,
    TransactionExecuted,
    ForwarderCallExecuted,
    CommitmentTreeRootAdded,
    KindTreeRootUpdated,
    LogicRefDenied,
    Paused,
    Unpaused,
    OwnershipTransferred,
    Initialized,
    Upgraded,
);

/// The event `body` encodes, which must be all of it.
fn event_body<E: AnchorDeserialize + Discriminator>(mut body: &[u8]) -> anyhow::Result<E> {
    let event = E::deserialize(&mut body).with_context(|| {
        format!(
            "the body of the event with discriminator {:02x?} does not decode",
            E::DISCRIMINATOR
        )
    })?;
    anyhow::ensure!(
        body.is_empty(),
        "{} bytes follow the body of the event with discriminator {:02x?}",
        body.len(),
        E::DISCRIMINATOR
    );
    Ok(event)
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol_adapter::events::event_instruction_data;

    #[test]
    fn an_event_the_adapter_encodes_decodes_to_itself() {
        let data = event_instruction_data(&protocol_adapter::ActionExecuted {
            action_tree_root: [1; 32],
            nullifiers: vec![[2; 32]],
            consumed_logic_refs: vec![[3; 32]],
            commitments: vec![[4; 32], [5; 32]],
            created_logic_refs: vec![[6; 32], [7; 32]],
        })
        .unwrap();
        let decoded = decode_adapter_event(&data).unwrap();
        let PaEvent::ActionExecuted(action) = &decoded else {
            panic!("{data:02x?} decoded to {decoded:?}, not an ActionExecuted");
        };
        assert_eq!(action.action_tree_root, [1; 32], "{decoded:?}");
        assert_eq!(action.nullifiers, [[2; 32]], "{decoded:?}");
        assert_eq!(action.consumed_logic_refs, [[3; 32]], "{decoded:?}");
        assert_eq!(action.commitments, [[4; 32], [5; 32]], "{decoded:?}");
        assert_eq!(action.created_logic_refs, [[6; 32], [7; 32]], "{decoded:?}");
    }

    #[test]
    fn events_of_one_layout_decode_by_their_own_discriminator() {
        let payload = |data: Vec<u8>| decode_adapter_event(&data).unwrap();
        let tag = [9; 32];
        let index = protocol_adapter::U256::from(3);
        let decoded = payload(
            event_instruction_data(&protocol_adapter::ExternalPayload {
                tag,
                index,
                blob: vec![1, 2],
            })
            .unwrap(),
        );
        assert!(
            matches!(&decoded, PaEvent::ExternalPayload(p) if p.tag == tag && p.index == index && p.blob == [1, 2]),
            "an ExternalPayload decoded to {decoded:?}"
        );
        let decoded = payload(
            event_instruction_data(&protocol_adapter::ApplicationPayload {
                tag,
                index,
                blob: vec![1, 2],
            })
            .unwrap(),
        );
        assert!(
            matches!(&decoded, PaEvent::ApplicationPayload(_)),
            "an ApplicationPayload decoded to {decoded:?}"
        );
    }

    #[test]
    fn data_without_the_event_tag_is_no_event() {
        let mut data = event_instruction_data(&protocol_adapter::TransactionExecuted {
            transaction_id: [1; 32],
        })
        .unwrap();
        data[0] ^= 1;
        let err = decode_adapter_event(&data).unwrap_err().to_string();
        assert!(
            err.contains("does not start with Anchor's event tag"),
            "{data:02x?}: {err}"
        );
    }

    #[test]
    fn an_unknown_discriminator_is_refused() {
        let mut data = event_instruction_data(&protocol_adapter::TransactionExecuted {
            transaction_id: [1; 32],
        })
        .unwrap();
        data[EVENT_IX_TAG_LE.len()] ^= 1;
        let err = decode_adapter_event(&data).unwrap_err().to_string();
        assert!(
            err.contains("the adapter emits no event whose discriminator starts"),
            "{data:02x?}: {err}"
        );
    }

    #[test]
    fn bytes_after_the_body_are_refused() {
        let mut data = event_instruction_data(&protocol_adapter::TransactionExecuted {
            transaction_id: [1; 32],
        })
        .unwrap();
        data.push(0);
        let err = decode_adapter_event(&data).unwrap_err().to_string();
        assert!(
            err.contains("1 bytes follow the body"),
            "{data:02x?}: {err}"
        );
    }

    #[test]
    fn a_truncated_body_is_refused() {
        let mut data = event_instruction_data(&protocol_adapter::TransactionExecuted {
            transaction_id: [1; 32],
        })
        .unwrap();
        data.pop();
        let err = decode_adapter_event(&data).unwrap_err().to_string();
        assert!(err.contains("does not decode"), "{data:02x?}: {err}");
    }
}
