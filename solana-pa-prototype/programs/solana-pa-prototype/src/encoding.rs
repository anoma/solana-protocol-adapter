//! Encoding utilities for word<->byte conversion and journal digest computation.

use crate::error::PAError;
use crate::merkle::{hash_two, PADDING_LEAF};
use crate::types::{Digest, LogicVerifierInputs, Transaction};

use arm_core::constants::COMPLIANCE_VK_BYTES;
pub use arm_core::utils::bytes_to_words;

/// Convert words to bytes (Vec variant for callers that need owned bytes).
pub fn words_to_bytes(words: &[u32]) -> Vec<u8> {
    arm_core::utils::words_to_bytes(words).to_vec()
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

    let mut size = len;
    while size > 1 {
        for i in 0..size / 2 {
            layer[i] = hash_two(&layer[2 * i], &layer[2 * i + 1]);
        }
        size /= 2;
    }

    Ok(layer[0])
}

/// Extract tags (nullifiers/commitments) and their expected logic refs from an action.
/// Returns (tags, logic_refs) where:
/// - tags[2i] = consumed_nullifier, tags[2i+1] = created_commitment
/// - logic_refs[2i] = consumed_logic_ref, logic_refs[2i+1] = created_logic_ref
pub fn extract_tags_and_logic_refs(
    action: &crate::types::Action,
) -> (Vec<Digest>, Vec<Digest>) {
    let mut tags = Vec::new();
    let mut logic_refs = Vec::new();

    for cu in &action.compliance_units {
        let instance = &cu.instance;
        tags.push(instance.consumed_nullifier);
        tags.push(instance.created_commitment);
        logic_refs.push(instance.consumed_logic_ref);
        logic_refs.push(instance.created_logic_ref);
    }

    (tags, logic_refs)
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
/// This builds and serializes the exact same tuple as the circuit:
/// `(Vec<ComplianceInstanceWords>, Digest, Vec<Vec<u32>>, Vec<Digest>)`
/// Then hashes the serialized bytes.
pub fn compute_batch_aggregation_journal_digest(tx: &Transaction) -> Result<Digest, PAError> {
    use anchor_lang::solana_program::hash::hash;
    use arm_core::compliance::ComplianceInstanceWords;

    // 1) Build Vec<ComplianceInstanceWords>
    let mut compliance_instances: Vec<ComplianceInstanceWords> = Vec::new();
    for action in &tx.actions {
        for cu in &action.compliance_units {
            compliance_instances.push(compliance_instance_to_words(&cu.instance));
        }
    }

    // 2) Compliance verifying key
    let compliance_key = Digest::from_bytes(COMPLIANCE_VK_BYTES);

    // 3) Build Vec<Vec<u32>> for logic instances and collect keys
    // Use the pre-serialized instance_journal (risc0 serde format) from each LogicVerifierInputs.
    // This is necessary because the logic circuits commit with risc0 serde, and we can't
    // reproduce that format on Solana without the risc0 crate.
    // NOTE: We intentionally do not deserialize journals on-chain to reconstruct AppData fields.
    // External-call execution consumes `logic_verifier_inputs.app_data` directly (EVM parity model).
    let mut logic_instances: Vec<Vec<u32>> = Vec::new();
    let mut logic_keys: Vec<Digest> = Vec::new();

    for action in &tx.actions {
        let (tags, expected_logic_refs) = extract_tags_and_logic_refs(action);

        if tags.len() != action.logic_verifier_inputs.len() {
            return Err(PAError::InvalidTransactionData);
        }

        for (tag, expected_vk) in tags.iter().zip(expected_logic_refs.iter()) {
            let input = find_logic_input(&action.logic_verifier_inputs, tag)?;
            if input.verifying_key != *expected_vk {
                return Err(PAError::InvalidTransactionData);
            }

            logic_instances.push(bytes_to_words(&input.instance_journal));
            logic_keys.push(input.verifying_key);
        }
    }

    // 4) Serialize the tuple exactly as the circuit does
    let output = (
        compliance_instances,
        compliance_key,
        logic_instances,
        logic_keys,
    );
    let journal_bytes = borsh::to_vec(&output).map_err(|_| PAError::InvalidTransactionData)?;

    // 5) Hash the journal bytes
    Ok(Digest::from_bytes(hash(&journal_bytes).to_bytes()))
}

/// Convert a ComplianceInstance to ComplianceInstanceWords (matching circuit format).
fn compliance_instance_to_words(
    instance: &crate::types::ComplianceInstance,
) -> arm_core::compliance::ComplianceInstanceWords {
    use arm_core::compliance::ComplianceInstanceWords;

    // Layout: consumed_nullifier(8) + consumed_logic_ref(8) + consumed_commitment_tree_root(8) +
    //         created_commitment(8) + created_logic_ref(8) + delta_x(8) + delta_y(8) = 56 words
    let mut words = [0u32; 56];
    words[0..8].copy_from_slice(&instance.consumed_nullifier.0);
    words[8..16].copy_from_slice(&instance.consumed_logic_ref.0);
    words[16..24].copy_from_slice(&instance.consumed_commitment_tree_root.0);
    words[24..32].copy_from_slice(&instance.created_commitment.0);
    words[32..40].copy_from_slice(&instance.created_logic_ref.0);
    words[40..48].copy_from_slice(&instance.delta_x);
    words[48..56].copy_from_slice(&instance.delta_y);

    ComplianceInstanceWords { u32_words: words }
}
