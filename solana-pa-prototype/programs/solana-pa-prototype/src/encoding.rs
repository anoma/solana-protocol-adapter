//! Encoding utilities for word<->byte conversion.
//!
//! Must match arm-risc0's bytes_to_words and words_to_bytes.

use crate::error::PAError;
use arm_types::{action::Action, utils::Digest};
use serde::Serialize;

/// Image ID for the compliance circuit (from `arm-risc0/arm/src/constants.rs`).
pub const COMPLIANCE_VK_BYTES: [u8; 32] =
    hex_literal::hex!("3003123ba707922b5a7124dccb3765cfb8a590852d4f25e29c9002f6efcfaa35");

/// Extract tags (nullifiers/commitments) and their expected logic refs from an action.
/// Returns (tags, logic_refs) where:
/// - tags[2i] = consumed_nullifier, tags[2i+1] = created_commitment
/// - logic_refs[2i] = consumed_logic_ref, logic_refs[2i+1] = created_logic_ref
pub fn extract_tags_and_logic_refs(action: &Action) -> Result<(Vec<Digest>, Vec<Digest>), PAError> {
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
