//! Property tests for serialization (types.rs coverage).
//!
//! Tests roundtrip serialization for core types and validates fixed size invariants.

use proptest::prelude::*;

use crate::proptests::strategies::{
    arb_compliance_instance, arb_digest, arb_expirable_blob, arb_solana_external_call,
};
use crate::types::{ComplianceInstance, Digest, ExpirableBlob, SolanaExternalCall};

/// Expected serialized size of ComplianceInstance:
/// 5 Digests (5 * 32 bytes) + 2 delta arrays (2 * 8 * 4 bytes) = 160 + 64 = 224 bytes
const COMPLIANCE_INSTANCE_SIZE: usize = 224;

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1000))]

    /// Digest bytes roundtrip: from_bytes -> to_bytes preserves data.
    #[test]
    fn prop_digest_bytes_roundtrip(bytes in prop::array::uniform32(any::<u8>())) {
        let digest = Digest::from_bytes(bytes);
        prop_assert_eq!(bytes, digest.to_bytes());
    }

    /// Digest hex roundtrip: from_hex -> to_bytes -> hex::encode preserves string.
    #[test]
    fn prop_digest_hex_roundtrip(digest in arb_digest()) {
        let hex_str = hex::encode(digest.to_bytes());
        let recovered = Digest::from_hex(&hex_str);
        prop_assert_eq!(digest.to_bytes(), recovered.to_bytes());
    }

    /// ComplianceInstance bincode roundtrip: serialize -> deserialize preserves all fields.
    #[test]
    fn prop_compliance_instance_roundtrip(instance in arb_compliance_instance()) {
        let serialized = bincode::serialize(&instance).unwrap();
        let deserialized: ComplianceInstance = bincode::deserialize(&serialized).unwrap();
        prop_assert_eq!(instance, deserialized);
    }

    /// ComplianceInstance has fixed serialized size of 224 bytes.
    #[test]
    fn prop_compliance_instance_fixed_size(instance in arb_compliance_instance()) {
        let serialized = bincode::serialize(&instance).unwrap();
        prop_assert_eq!(
            serialized.len(),
            COMPLIANCE_INSTANCE_SIZE,
            "ComplianceInstance serialized size must be {} bytes, got {}",
            COMPLIANCE_INSTANCE_SIZE,
            serialized.len()
        );
    }

    /// ExpirableBlob bincode roundtrip: serialize -> deserialize preserves data.
    #[test]
    fn prop_expirable_blob_roundtrip(blob in arb_expirable_blob(64)) {
        let serialized = bincode::serialize(&blob).unwrap();
        let deserialized: ExpirableBlob = bincode::deserialize(&serialized).unwrap();
        prop_assert_eq!(blob.blob, deserialized.blob);
        prop_assert_eq!(blob.deletion_criterion, deserialized.deletion_criterion);
    }

    /// SolanaExternalCall bincode roundtrip: serialize -> deserialize preserves all fields.
    #[test]
    fn prop_external_call_roundtrip(call in arb_solana_external_call(256)) {
        let serialized = bincode::serialize(&call).unwrap();
        let deserialized: SolanaExternalCall = bincode::deserialize(&serialized).unwrap();
        prop_assert_eq!(call, deserialized);
    }
}
