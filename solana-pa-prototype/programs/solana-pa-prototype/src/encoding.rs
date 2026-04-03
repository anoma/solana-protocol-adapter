//! Journal digest computation and action tree utilities.

use crate::error::PAError;
use crate::merkle::{hash_two, PADDING_LEAF};
use arm_core::logic_instance::LogicVerifierInputs;
use arm_core::transaction::Transaction;
use arm_core::Digest;

use arm_core::constants::COMPLIANCE_VK_BYTES;
use arm_core::utils::bytes_to_words;

/// Compute the journal digest for verifying a *batch aggregation* proof.
///
/// This builds and serializes the exact same tuple as the circuit:
/// `(Vec<ComplianceInstanceWords>, Digest, Vec<Vec<u32>>, Vec<Digest>)`
/// Then hashes the serialized bytes.
pub fn compute_batch_aggregation_journal_digest(tx: &Transaction) -> Result<Digest, PAError> {
    use anchor_lang::solana_program::hash::hash;
    use arm_core::compliance::ComplianceInstanceWords;

    let total_cus: usize = tx.actions.iter().map(|a| a.compliance_units.len()).sum();
    let total_tags = total_cus * 2;

    let mut compliance_instances: Vec<ComplianceInstanceWords> = Vec::with_capacity(total_cus);
    let mut logic_instances: Vec<Vec<u32>> = Vec::with_capacity(total_tags);
    let mut logic_keys: Vec<Digest> = Vec::with_capacity(total_tags);

    for action in &tx.actions {
        for cu in &action.compliance_units {
            compliance_instances.push(ComplianceInstanceWords::from(&cu.instance));
        }

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

    // Serialize the tuple exactly as the circuit does, then hash
    let compliance_key = Digest::from_bytes(COMPLIANCE_VK_BYTES);
    let output = (
        compliance_instances,
        compliance_key,
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

/// Verify that each LVI's app_data matches the hash proven in its instance_journal.
///
/// The logic circuit guest computes `sha256(borsh(app_data))` and commits it as the
/// last 32 bytes of the journal. This function recomputes the hash on-chain and
/// compares it against the proven value, preventing app_data substitution attacks.
pub fn verify_app_data_hashes(tx: &Transaction) -> Result<(), PAError> {
    use anchor_lang::solana_program::hash::hash;

    for action in &tx.actions {
        for lvi in &action.logic_verifier_inputs {
            let journal = &lvi.instance_journal;
            // The app_data_hash Digest is the last field of LogicInstance.
            // It occupies 8 u32 words = 32 bytes at the end of the journal.
            // The journal is 4-byte aligned (risc0 serde produces u32 words;
            // borsh to_journal() pads to alignment before the hash).
            if journal.len() < 32 || journal.len() % 4 != 0 {
                return Err(PAError::AppDataHashMismatch);
            }

            // Extract the last 8 u32 words as the proven hash.
            // Journal bytes are native-endian u32 words.
            let hash_start = journal.len() - 32;
            let mut proven_words = [0u32; 8];
            for (i, word) in proven_words.iter_mut().enumerate() {
                let off = hash_start + i * 4;
                *word = u32::from_ne_bytes(
                    journal[off..off + 4]
                        .try_into()
                        .map_err(|_| PAError::AppDataHashMismatch)?,
                );
            }

            // Compute the expected hash: sha256(borsh(app_data)), converted
            // to Digest word representation via from_bytes (same as compute_hash).
            let app_data_bytes =
                borsh::to_vec(&lvi.app_data).map_err(|_| PAError::InvalidTransactionData)?;
            let computed_hash = hash(&app_data_bytes);
            let computed_digest = Digest::from_bytes(computed_hash.to_bytes());

            if proven_words != *computed_digest.as_words() {
                return Err(PAError::AppDataHashMismatch);
            }
        }
    }
    Ok(())
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
