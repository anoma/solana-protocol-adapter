//! Tests for Ed25519 signature verification

use crate::ed25519::SIGNATURE_OFFSETS_SERIALIZED_SIZE;

// Ed25519 instruction introspection is difficult to unit test
// without mocking the sysvar. Integration tests will cover this.

#[test]
fn test_offset_size() {
    // Verify our constant matches the expected size
    assert_eq!(SIGNATURE_OFFSETS_SERIALIZED_SIZE, 14);
}
