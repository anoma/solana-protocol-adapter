//! A 256-bit unsigned integer for the event fields pa-evm declares
//! `uint256`. Borsh writes it as 32 little-endian bytes, the encoding of the
//! IDL's `u256`, which Anchor's IDL builder has no Rust type to produce: the
//! IDL names it as a type alias of `u256`.

// Borsh's own derives: Anchor's would also give the type an `IdlBuild` as a
// struct of 32 bytes, in place of the alias below.
#[derive(borsh::BorshSerialize, borsh::BorshDeserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct U256([u8; 32]);

impl From<usize> for U256 {
    fn from(value: usize) -> Self {
        let mut bytes = [0u8; 32];
        bytes[..core::mem::size_of::<usize>()].copy_from_slice(&value.to_le_bytes());
        Self(bytes)
    }
}

#[cfg(feature = "idl-build")]
impl anchor_lang::IdlBuild for U256 {
    fn create_type() -> Option<anchor_lang::idl::types::IdlTypeDef> {
        use anchor_lang::idl::types::{IdlType, IdlTypeDef, IdlTypeDefTy};
        Some(IdlTypeDef {
            name: "U256".into(),
            docs: vec!["A 256-bit unsigned integer: 32 little-endian bytes.".into()],
            serialization: Default::default(),
            repr: None,
            generics: vec![],
            ty: IdlTypeDefTy::Type {
                alias: IdlType::U256,
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_index_is_its_little_endian_bytes_padded_to_32() {
        let encoded = borsh::to_vec(&U256::from(0x0102_0304usize)).unwrap();
        let mut expected = [0u8; 32];
        expected[..4].copy_from_slice(&[0x04, 0x03, 0x02, 0x01]);
        assert_eq!(encoded, expected);
    }
}
