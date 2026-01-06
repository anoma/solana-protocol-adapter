//! Unit tests for types module.

use crate::types::ExpirableBlob;

#[test]
fn test_expirable_blob_serialization() {
    let blob = ExpirableBlob {
        blob: vec![0x01020304, 0x05060708],
        deletion_criterion: 42,
    };
    let serialized = bincode::serialize(&blob).unwrap();
    let deserialized: ExpirableBlob = bincode::deserialize(&serialized).unwrap();
    assert_eq!(blob.blob, deserialized.blob);
    assert_eq!(blob.deletion_criterion, deserialized.deletion_criterion);
}
