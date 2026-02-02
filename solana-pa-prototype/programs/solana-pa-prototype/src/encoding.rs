//! Encoding utilities for word<->byte conversion.
//!
//! Shared utilities are imported from arm-risc0 where possible.

use alloc::vec::Vec;

use crate::error::PAError;
use crate::merkle::{hash_two, PADDING_LEAF};
use crate::types::{Digest, LogicVerifierInputs, Transaction};
use anoma_rm_risc0::logic_instance::LogicInstance;

// Re-export bytes_to_words from arm-risc0
pub use anoma_rm_risc0::utils::bytes_to_words;

/// Convert words to bytes using little-endian byte order.
/// Wrapper around arm-risc0's words_to_bytes that returns Vec<u8> for compatibility.
pub fn words_to_bytes(words: &[u32]) -> Vec<u8> {
    anoma_rm_risc0::utils::words_to_bytes(words).to_vec()
}

// Re-export Solana-specific constants from arm-risc0
pub use anoma_rm_risc0::solana_constants::COMPLIANCE_VK_BYTES;

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
        let instance = &cu.instance;
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
/// This uses Borsh serialization to match the Solana-variant guest program output.
/// The journal contains:
/// 1. Vec<ComplianceInstance> - all compliance instances
/// 2. Digest - compliance verifying key
/// 3. Vec<LogicInstance> - all logic instances
/// 4. Vec<Digest> - all logic verifying keys
pub fn compute_batch_aggregation_journal_digest(tx: &Transaction) -> Result<Digest, PAError> {
    use anchor_lang::solana_program::hash::Hasher;

    let compliance_count: usize = tx.actions.iter().map(|a| a.compliance_units.len()).sum();
    let logic_count = compliance_count
        .checked_mul(2)
        .ok_or(PAError::InvalidTransactionData)?;

    let mut hasher = Hasher::default();

    // 1) Vec<ComplianceInstance> - serialize count then each instance
    let count_bytes = (compliance_count as u32).to_le_bytes();
    hasher.hash(&count_bytes);

    for action in &tx.actions {
        for cu in &action.compliance_units {
            let instance_bytes = borsh::to_vec(&cu.instance)
                .map_err(|_| PAError::InvalidTransactionData)?;
            hasher.hash(&instance_bytes);
        }
    }

    // 2) Digest - compliance verifying key
    let compliance_vk = Digest::from_bytes(COMPLIANCE_VK_BYTES);
    let vk_bytes = borsh::to_vec(&compliance_vk)
        .map_err(|_| PAError::InvalidTransactionData)?;
    hasher.hash(&vk_bytes);

    // 3) Vec<LogicInstance> - serialize count then each instance
    let logic_count_bytes = (logic_count as u32).to_le_bytes();
    hasher.hash(&logic_count_bytes);

    let mut logic_keys: Vec<Digest> = Vec::with_capacity(logic_count);

    for action in &tx.actions {
        let mut tags: Vec<Digest> = Vec::new();
        let mut logics: Vec<Digest> = Vec::new();

        for cu in &action.compliance_units {
            let instance = &cu.instance;
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

            let logic_instance = LogicInstance {
                tag: input.tag,
                is_consumed: index % 2 == 0,
                root: action_tree_root,
                app_data: input.app_data.clone(),
            };

            let instance_bytes = borsh::to_vec(&logic_instance)
                .map_err(|_| PAError::InvalidTransactionData)?;
            hasher.hash(&instance_bytes);

            logic_keys.push(input.verifying_key);
        }
    }

    // 4) Vec<Digest> - logic verifying keys
    let keys_count_bytes = (logic_keys.len() as u32).to_le_bytes();
    hasher.hash(&keys_count_bytes);

    for vk in logic_keys {
        let vk_bytes = borsh::to_vec(&vk)
            .map_err(|_| PAError::InvalidTransactionData)?;
        hasher.hash(&vk_bytes);
    }

    Ok(Digest::from_bytes(hasher.result().to_bytes()))
}
