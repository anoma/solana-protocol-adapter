use crate::error::PAError;
use crate::external_calls::{decode_external_call, verify_output};
use arm_core::logic_instance::ExpirableBlob;

#[test]
fn test_error_external_call_output_mismatch() {
    let expected = vec![1, 2, 3];
    let actual = vec![4, 5, 6];

    let result = verify_output(&expected, &actual);
    assert!(matches!(result, Err(PAError::ForwarderCallOutputMismatch)));
}

#[test]
fn test_error_invalid_external_call_blob() {
    let blob = ExpirableBlob {
        blob: vec![0xDEAD, 0xBEEF],
        deletion_criterion: 0,
    };

    let result = decode_external_call(&blob);
    assert!(matches!(result, Err(PAError::InvalidExternalCallBlob)));
}

/// Settlement's only presence check for the aggregation is
/// `require_aggregation`; its error must surface as `AggregationRequired`.
#[test]
fn missing_aggregation_maps_to_aggregation_required() {
    let mut tx = crate::tests::utils::create_minimal_transaction();
    tx.aggregation = None;
    let err = arm_solana::journal::require_aggregation(&tx)
        .map_err(PAError::from)
        .unwrap_err();
    assert!(
        matches!(err, PAError::AggregationRequired),
        "expected AggregationRequired, got {err:?}"
    );
}
