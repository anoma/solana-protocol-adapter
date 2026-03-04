//! Property tests for delta verification.

use super::strategies::arb_compliance_instance;
use crate::delta::verify_delta_proof;
use crate::tests::utils::build_tx_from_instances;
use crate::types::{Delta, DeltaWitness};
use proptest::prelude::*;

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1000))]

    /// Property: witness delta_proof is rejected by verify_delta_proof.
    #[test]
    fn prop_witness_rejected(inst in arb_compliance_instance()) {
        let mut tx = build_tx_from_instances(&[inst]);
        tx.delta_proof = Delta::Witness(DeltaWitness([1u8; 32]));

        let result = verify_delta_proof(&tx);
        prop_assert!(result.is_err(), "witness delta_proof should be rejected");
    }
}
