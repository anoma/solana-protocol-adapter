//! Settlement extraction helpers for processing RM transactions.

use crate::error::PAError;
use crate::external_calls::decode_external_call;
use crate::types::{Digest, SolanaExternalCall, Transaction};

/// Extract nullifiers from a transaction by parsing each ComplianceUnit.instance.
pub fn extract_nullifiers(tx: &Transaction) -> Vec<Digest> {
    let mut nullifiers = Vec::new();
    for action in &tx.actions {
        for cu in &action.compliance_units {
            nullifiers.push(cu.instance.consumed_nullifier);
        }
    }
    nullifiers
}

/// Extract commitments from a transaction by parsing each ComplianceUnit.instance.
pub fn extract_commitments(tx: &Transaction) -> Vec<Digest> {
    let mut commitments = Vec::new();
    for action in &tx.actions {
        for cu in &action.compliance_units {
            commitments.push(cu.instance.created_commitment);
        }
    }
    commitments
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
