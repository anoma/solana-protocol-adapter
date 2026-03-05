//! Settlement extraction helpers for processing RM transactions.

use arm_core::compliance::ComplianceInstance;
use arm_core::transaction::Transaction;
use arm_core::Digest;

/// Extract a field from every ComplianceInstance across all actions.
fn extract_from_compliance_units(
    tx: &Transaction,
    field: fn(&ComplianceInstance) -> Digest,
) -> Vec<Digest> {
    tx.actions
        .iter()
        .flat_map(|a| a.compliance_units.iter())
        .map(|cu| field(&cu.instance))
        .collect()
}

/// Extract nullifiers from a transaction by parsing each ComplianceUnit.instance.
pub fn extract_nullifiers(tx: &Transaction) -> Vec<Digest> {
    extract_from_compliance_units(tx, |i| i.consumed_nullifier)
}

/// Extract commitments from a transaction by parsing each ComplianceUnit.instance.
pub fn extract_commitments(tx: &Transaction) -> Vec<Digest> {
    extract_from_compliance_units(tx, |i| i.created_commitment)
}
