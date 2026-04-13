//! Journal digest computation and action tree utilities.

use crate::error::PAError;
use crate::merkle::{hash_two, PADDING_LEAF};
use arm_core::logic_instance::LogicVerifierInputs;
use arm_core::transaction::Transaction;
use arm_core::Digest;

use arm_core::constants::COMPLIANCE_VK_BYTES;
use arm_core::utils::bytes_to_words;

/// Compute the journal digest pinned by the batch aggregation Groth16 proof.
///
/// Per-LVI journal bytes are re-derived from `lvi.app_data` via
/// `LogicInstance::to_journal()`, so any mutation of `app_data` flows into the
/// digest and invalidates the proof (H-001 closure).
pub fn compute_batch_aggregation_journal_digest(tx: &Transaction) -> Result<Digest, PAError> {
    use anchor_lang::solana_program::hash::hash;
    use arm_core::compliance::ComplianceInstanceWords;

    let mut compliance_instances: Vec<ComplianceInstanceWords> = Vec::new();
    let mut logic_instances: Vec<Vec<u32>> = Vec::new();
    let mut logic_keys: Vec<Digest> = Vec::new();

    for action in &tx.actions {
        for cu in &action.compliance_units {
            compliance_instances.push(ComplianceInstanceWords::from(&cu.instance));
        }

        let (tags, expected_logic_refs) = extract_tags_and_logic_refs(action);
        if tags.len() != action.logic_verifier_inputs.len() {
            return Err(PAError::InvalidTransactionData);
        }

        let action_tree_root = compute_action_tree_root(&tags)?;

        // tags is [consumed_nullifier, created_commitment, ...] so even idx is consumed.
        for (idx, (tag, expected_vk)) in tags.iter().zip(&expected_logic_refs).enumerate() {
            let input = find_logic_input(&action.logic_verifier_inputs, tag)?;
            if input.verifying_key != *expected_vk {
                return Err(PAError::InvalidTransactionData);
            }
            let journal_bytes = input
                .to_instance(idx % 2 == 0, action_tree_root)
                .to_journal()
                .map_err(|_| PAError::InvalidTransactionData)?;
            logic_instances.push(bytes_to_words(&journal_bytes));
            logic_keys.push(input.verifying_key);
        }
    }

    // borsh and risc0_zkvm::serde agree byte-for-byte on this tuple shape:
    // both use 4-byte LE length prefixes for Vec and identical [u32; N] layout.
    let output = (
        compliance_instances,
        Digest::from_bytes(COMPLIANCE_VK_BYTES),
        logic_instances,
        logic_keys,
    );
    let journal_bytes = borsh::to_vec(&output).map_err(|_| PAError::InvalidTransactionData)?;
    Ok(Digest::from_bytes(hash(&journal_bytes).to_bytes()))
}

/// Compute the action tree root from a list of tags.
/// Uses a power-of-2 balanced merkle tree with PADDING_LEAF for missing nodes.
pub fn compute_action_tree_root(tags: &[Digest]) -> Result<Digest, PAError> {
    if tags.is_empty() {
        return Err(PAError::InvalidTransactionData);
    }

    let len = tags
        .len()
        .checked_next_power_of_two()
        .ok_or(PAError::InvalidTransactionData)?;
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
    action: &arm_core::action::Action,
) -> (Vec<Digest>, Vec<Digest>) {
    let cap = 2 * action.compliance_units.len();
    let mut tags = Vec::with_capacity(cap);
    let mut logic_refs = Vec::with_capacity(cap);

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
pub fn find_logic_input<'a>(
    inputs: &'a [LogicVerifierInputs],
    tag: &Digest,
) -> Result<&'a LogicVerifierInputs, PAError> {
    inputs
        .iter()
        .find(|lvi| &lvi.tag == tag)
        .ok_or(PAError::TagNotFound)
}
