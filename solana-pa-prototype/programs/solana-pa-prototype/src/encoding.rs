//! Encoding utilities for word<->byte conversion.
//!
//! Must match arm-risc0's bytes_to_words and words_to_bytes.

use alloc::vec::Vec;

use crate::error::PAError;
use crate::merkle::{hash_two, PADDING_LEAF};
use crate::risc0_serde;
use crate::types::{AppData, Digest, LogicVerifierInputs, Transaction};
use serde::Serialize;

/// Convert bytes to words (matching arm-risc0).
/// Pads with zeros to word boundary. Uses little-endian byte order.
pub fn bytes_to_words(bytes: &[u8]) -> Vec<u32> {
    let padded_len = (bytes.len() + 3) / 4 * 4;
    let mut padded = bytes.to_vec();
    padded.resize(padded_len, 0);
    padded
        .chunks_exact(4)
        .map(|chunk| u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .collect()
}

/// Convert words to bytes using little-endian byte order.
pub fn words_to_bytes(words: &[u32]) -> Vec<u8> {
    words.iter().flat_map(|w| w.to_le_bytes()).collect()
}

/// Image ID for the compliance circuit (from `arm-risc0/arm/src/constants.rs`).
pub const COMPLIANCE_VK_BYTES: [u8; 32] =
    hex_literal::hex!("3003123ba707922b5a7124dccb3765cfb8a590852d4f25e29c9002f6efcfaa35");

#[derive(Clone, Debug)]
struct U32Array56([u32; 56]);

impl serde::Serialize for U32Array56 {
    fn serialize<S>(&self, serializer: S) -> core::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeTuple;

        let mut tuple = serializer.serialize_tuple(56)?;
        for word in self.0.iter() {
            tuple.serialize_element(word)?;
        }
        tuple.end()
    }
}

#[derive(Clone, Debug, serde::Serialize)]
struct ComplianceInstanceWords {
    pub u32_words: U32Array56,
}

/// Parse a bincode-serialized ComplianceInstance.
pub fn parse_compliance_instance(
    instance_bytes: &[u8],
) -> Result<crate::types::ComplianceInstance, PAError> {
    bincode::deserialize(instance_bytes).map_err(|_| PAError::InvalidTransactionData)
}

fn next_power_of_two(n: usize) -> Result<usize, PAError> {
    n.checked_next_power_of_two()
        .ok_or(PAError::InvalidTransactionData)
}

/// Compute the action tree root from a list of tags.
/// Uses a power-of-2 balanced merkle tree with PADDING_LEAF for missing nodes.
/// Used for journal digest computation in `compute_batch_aggregation_journal_digest`.
pub fn compute_action_tree_root(tags: &[Digest]) -> Result<Digest, PAError> {
    if tags.is_empty() {
        return Err(PAError::InvalidTransactionData);
    }

    let len = next_power_of_two(tags.len())?;
    let mut layer: Vec<Digest> = tags.to_vec();
    layer.resize(len, PADDING_LEAF);

    while layer.len() > 1 {
        let mut next = Vec::with_capacity(layer.len() / 2);
        for pair in layer.chunks_exact(2) {
            next.push(hash_two(&pair[0], &pair[1]));
        }
        layer = next;
    }

    Ok(layer[0])
}

/// Extract tags (nullifiers/commitments) and their expected logic refs from an action.
/// Returns (tags, logic_refs) where:
/// - tags[2i] = consumed_nullifier, tags[2i+1] = created_commitment
/// - logic_refs[2i] = consumed_logic_ref, logic_refs[2i+1] = created_logic_ref
pub fn extract_tags_and_logic_refs(
    action: &crate::types::Action,
) -> Result<(Vec<Digest>, Vec<Digest>), PAError> {
    let mut tags = Vec::new();
    let mut logic_refs = Vec::new();

    for cu in &action.compliance_units {
        let instance = parse_compliance_instance(&cu.instance)?;
        tags.push(instance.consumed_nullifier);
        tags.push(instance.created_commitment);
        logic_refs.push(instance.consumed_logic_ref);
        logic_refs.push(instance.created_logic_ref);
    }

    Ok((tags, logic_refs))
}

/// Find a LogicVerifierInputs entry by its tag.
/// Used for journal digest computation in `compute_batch_aggregation_journal_digest`.
pub fn find_logic_input<'a>(
    inputs: &'a [LogicVerifierInputs],
    tag: &Digest,
) -> Result<&'a LogicVerifierInputs, PAError> {
    inputs
        .iter()
        .find(|lvi| &lvi.tag == tag)
        .ok_or(PAError::TagNotFound)
}

/// Compute the journal digest for verifying a *batch aggregation* proof.
///
/// This matches `arm-risc0/arm/src/aggregation/batch.rs::verify_transaction_aggregation`.
pub fn compute_batch_aggregation_journal_digest(tx: &Transaction) -> Result<Digest, PAError> {
    use anchor_lang::solana_program::hash::Hasher;
    use serde::ser::Serializer as _;

    #[derive(Clone, Debug, serde::Serialize)]
    struct LogicInstanceRef<'a> {
        pub tag: Digest,
        pub is_consumed: bool,
        pub root: Digest,
        pub app_data: &'a AppData,
    }

    struct HasherWordWriter<'a> {
        hasher: &'a mut Hasher,
    }

    impl<'a> risc0_serde::WordWrite for HasherWordWriter<'a> {
        fn write_words(&mut self, words: &[u32]) -> risc0_serde::Result<()> {
            for word in words {
                self.hasher.hash(&word.to_le_bytes());
            }
            Ok(())
        }

        fn write_padded_bytes(&mut self, bytes: &[u8]) -> risc0_serde::Result<()> {
            let mut offset = 0usize;
            while offset + 4 <= bytes.len() {
                self.hasher.hash(&bytes[offset..offset + 4]);
                offset += 4;
            }
            if offset < bytes.len() {
                let mut last = [0u8; 4];
                last[..bytes.len() - offset].copy_from_slice(&bytes[offset..]);
                self.hasher.hash(&last);
            }
            Ok(())
        }
    }

    let compliance_count: usize = tx.actions.iter().map(|a| a.compliance_units.len()).sum();
    let logic_count = compliance_count
        .checked_mul(2)
        .ok_or(PAError::InvalidTransactionData)?;

    let mut hasher = Hasher::default();
    let mut writer = HasherWordWriter {
        hasher: &mut hasher,
    };
    let mut serializer = risc0_serde::Serializer::new(&mut writer);

    // 1) Vec<ComplianceInstanceWords>
    serializer
        .serialize_u32(compliance_count as u32)
        .map_err(|_| PAError::InvalidTransactionData)?;
    for action in &tx.actions {
        for cu in &action.compliance_units {
            let words = bytes_to_words(&cu.instance);
            let fixed: [u32; 56] = words
                .try_into()
                .map_err(|_| PAError::InvalidTransactionData)?;
            let element = ComplianceInstanceWords {
                u32_words: U32Array56(fixed),
            };
            element
                .serialize(&mut serializer)
                .map_err(|_| PAError::InvalidTransactionData)?;
        }
    }

    // 2) compliance_vk Digest
    let compliance_vk = Digest::from_bytes(COMPLIANCE_VK_BYTES);
    compliance_vk
        .serialize(&mut serializer)
        .map_err(|_| PAError::InvalidTransactionData)?;

    // 3) Vec<Vec<u32>> logic_instances (each inner Vec is the LogicInstance word stream)
    serializer
        .serialize_u32(logic_count as u32)
        .map_err(|_| PAError::InvalidTransactionData)?;

    let mut logic_keys: Vec<Digest> = Vec::with_capacity(logic_count);
    for action in &tx.actions {
        let mut tags: Vec<Digest> = Vec::new();
        let mut logics: Vec<Digest> = Vec::new();

        for cu in &action.compliance_units {
            let instance = parse_compliance_instance(&cu.instance)?;
            tags.push(instance.consumed_nullifier);
            tags.push(instance.created_commitment);
            logics.push(instance.consumed_logic_ref);
            logics.push(instance.created_logic_ref);
        }

        let action_tree_root = compute_action_tree_root(&tags)?;

        if tags.len() != action.logic_verifier_inputs.len() {
            return Err(PAError::InvalidTransactionData);
        }

        for (index, (tag, expected_vk)) in tags.iter().zip(logics.iter()).enumerate() {
            let input = find_logic_input(&action.logic_verifier_inputs, tag)?;
            if input.verifying_key != *expected_vk {
                return Err(PAError::InvalidTransactionData);
            }

            let instance = LogicInstanceRef {
                tag: input.tag,
                is_consumed: index % 2 == 0,
                root: action_tree_root,
                app_data: &input.app_data,
            };

            let word_len =
                risc0_serde::count_words(&instance).map_err(|_| PAError::InvalidTransactionData)?;

            serializer
                .serialize_u32(word_len as u32)
                .map_err(|_| PAError::InvalidTransactionData)?;
            instance
                .serialize(&mut serializer)
                .map_err(|_| PAError::InvalidTransactionData)?;

            logic_keys.push(input.verifying_key);
        }
    }

    // 4) Vec<Digest> logic_keys
    serializer
        .serialize_u32(logic_keys.len() as u32)
        .map_err(|_| PAError::InvalidTransactionData)?;
    for vk in logic_keys {
        vk.serialize(&mut serializer)
            .map_err(|_| PAError::InvalidTransactionData)?;
    }

    Ok(Digest::from_bytes(hasher.result().to_bytes()))
}
