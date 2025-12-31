//! Settlement extraction helpers for processing RM transactions.

use crate::error::PAError;
use crate::types::{Digest, SolanaExternalCall, Transaction};
use crate::external_calls::decode_external_call;
use crate::journal::parse_compliance_instance;

/// Extract nullifiers from a transaction by parsing each ComplianceUnit.instance.
/// This matches arm-risc0's nf_duplication_check behavior:
/// nullifier = cu.get_instance()?.consumed_nullifier
pub fn extract_nullifiers(tx: &Transaction) -> Result<Vec<Digest>, PAError> {
    let mut nullifiers = Vec::new();
    for action in &tx.actions {
        for cu in &action.compliance_units {
            let instance = parse_compliance_instance(&cu.instance)?;
            nullifiers.push(instance.consumed_nullifier);
        }
    }
    Ok(nullifiers)
}

/// Extract commitments from a transaction by parsing each ComplianceUnit.instance.
/// commitment = cu.get_instance()?.created_commitment
pub fn extract_commitments(tx: &Transaction) -> Result<Vec<Digest>, PAError> {
    let mut commitments = Vec::new();
    for action in &tx.actions {
        for cu in &action.compliance_units {
            let instance = parse_compliance_instance(&cu.instance)?;
            commitments.push(instance.created_commitment);
        }
    }
    Ok(commitments)
}

/// Extract external calls from a transaction.
///
/// Iterates through all actions and their LogicVerifierInputs, decoding each
/// external_payload blob as a SolanaExternalCall.
///
/// Returns a vec of (logic_ref, call) tuples where logic_ref is the verifying_key
/// from the LogicVerifierInputs containing the call.
pub fn extract_external_calls(tx: &Transaction) -> Result<Vec<(Digest, SolanaExternalCall)>, PAError> {
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

