//! A confirmed transaction as a test reads it back: the transaction as sent,
//! the keys its lookup tables loaded, the runtime's log of it, and the
//! instructions its programs invoked, among them the events a program emits
//! by invoking itself (Anchor's `emit_cpi!`).

use anoma_pa_solana_client::EVENT_IX_TAG;
use anoma_pa_solana_client::events::{PaEvent, decode_event_instruction};
use anyhow::Context;
use solana_commitment_config::CommitmentConfig;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_rpc_client_types::config::RpcTransactionConfig;
use solana_signature::Signature;
use solana_transaction::versioned::VersionedTransaction;
use solana_transaction_status_client_types::{
    UiInnerInstructions, UiInstruction, UiLoadedAddresses, UiTransactionEncoding,
};
use surfpool_sdk::Pubkey;

/// What the runtime recorded of one confirmed transaction.
pub struct Executed {
    /// The transaction as it was sent.
    pub transaction: VersionedTransaction,
    /// The keys its lookup tables loaded, writable before read-only.
    pub loaded: Vec<Pubkey>,
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
                    max_supported_transaction_version: Some(0),
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

        let addresses: Option<UiLoadedAddresses> = meta.loaded_addresses.into();
        let loaded = match addresses {
            Some(addresses) => addresses
                .writable
                .iter()
                .chain(&addresses.readonly)
                .map(|key| {
                    key.parse()
                        .with_context(|| format!("the loaded address {key} is not base58"))
                })
                .collect::<anyhow::Result<Vec<Pubkey>>>()?,
            None => Vec::new(),
        };
        // An instruction names its program by index into the message's keys,
        // then the loaded keys.
        let keys = [transaction.message.static_account_keys(), &loaded].concat();
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
            loaded,
            logs: logs.unwrap_or_default(),
            inner,
        })
    }

    /// The instruction data of every event `program` emitted, in order: its
    /// invocations of itself that carry Anchor's event tag.
    pub fn cpi_events<'a>(&'a self, program: &'a Pubkey) -> impl Iterator<Item = &'a [u8]> {
        self.inner
            .iter()
            .filter(move |(invoked, data)| invoked == program && data.starts_with(&EVENT_IX_TAG))
            .map(|(_, data)| data.as_slice())
    }

    /// The events the adapter `program` emitted, decoded, in order.
    pub fn adapter_events(&self, program: &Pubkey) -> anyhow::Result<Vec<PaEvent>> {
        Ok(self
            .cpi_events(program)
            .map(decode_event_instruction)
            .collect::<Result<_, _>>()?)
    }
}
