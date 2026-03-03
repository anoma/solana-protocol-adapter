//! Settlement extraction helpers for processing RM transactions.

use crate::error::PAError;
use crate::external_calls::decode_external_call;
use crate::types::{ComplianceInstance, Digest, SolanaExternalCall, Transaction};

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

/// Extract external calls from a transaction.
///
/// Iterates through all actions and their LogicVerifierInputs, decoding each
/// external_payload blob as a SolanaExternalCall.
///
/// Returns a vec of (logic_ref, call) tuples where logic_ref is the verifying_key
/// from the LogicVerifierInputs containing the call.
pub fn extract_external_calls(
    tx: &Transaction,
) -> Result<Vec<(Digest, SolanaExternalCall)>, PAError> {
    let mut calls = Vec::new();
    for action in &tx.actions {
        for lvi in &action.logic_verifier_inputs {
            let logic_ref = lvi.verifying_key;
            for blob in &lvi.app_data.external_payload {
                let call = decode_external_call(blob)?;
                calls.push((logic_ref, call));
            }
        }
    }
    Ok(calls)
}
