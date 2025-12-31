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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::{
        create_compliance_instance, create_transaction_with_compliance_instances,
    };

    #[test]
    fn test_extract_nullifiers_from_compliance_instances() {
        // Create transaction with proper ComplianceUnit.instance bytes
        let nullifier1 = Digest::from_bytes([1u8; 32]);
        let nullifier2 = Digest::from_bytes([2u8; 32]);

        let tx = create_transaction_with_compliance_instances(vec![
            create_compliance_instance(nullifier1, Digest::from_bytes([0xA1; 32])),
            create_compliance_instance(nullifier2, Digest::from_bytes([0xA2; 32])),
        ]);

        let nullifiers = extract_nullifiers(&tx).unwrap();
        assert_eq!(nullifiers.len(), 2);
        assert_eq!(nullifiers[0], nullifier1);
        assert_eq!(nullifiers[1], nullifier2);
    }

    #[test]
    fn test_extract_commitments_from_compliance_instances() {
        let commitment1 = Digest::from_bytes([0xC1; 32]);
        let commitment2 = Digest::from_bytes([0xC2; 32]);

        let tx = create_transaction_with_compliance_instances(vec![
            create_compliance_instance(Digest::from_bytes([1u8; 32]), commitment1),
            create_compliance_instance(Digest::from_bytes([2u8; 32]), commitment2),
        ]);

        let commitments = extract_commitments(&tx).unwrap();
        assert_eq!(commitments.len(), 2);
        assert_eq!(commitments[0], commitment1);
        assert_eq!(commitments[1], commitment2);
    }

    #[test]
    fn test_settle_extracts_nullifiers() {
        // Note: Nullifier PDA creation is tested in integration tests.
        // This test verifies nullifier extraction works correctly.
        let nullifier = Digest::from_bytes([0xBB; 32]);
        let tx = create_transaction_with_compliance_instances(vec![create_compliance_instance(
            nullifier,
            Digest::from_bytes([0xC1; 32]),
        )]);

        let extracted = extract_nullifiers(&tx).unwrap();
        assert_eq!(extracted.len(), 1);
        assert_eq!(extracted[0], nullifier);
    }
}
