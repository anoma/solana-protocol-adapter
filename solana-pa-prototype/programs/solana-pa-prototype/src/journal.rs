//! Journal digest computation for risc0-solana verification.

use crate::error::PAError;
use crate::types::ComplianceInstance;

/// Parse a ComplianceInstance from raw journal bytes.
/// This mirrors arm-risc0's journal_to_instance function.
pub fn parse_compliance_instance(instance_bytes: &[u8]) -> Result<ComplianceInstance, PAError> {
    bincode::deserialize(instance_bytes).map_err(|_| PAError::InvalidProof)
}

/// LogicInstance for non-aggregated verification.
/// Matches arm-risc0's LogicInstance structure.
#[cfg(feature = "non-aggregated-proofs")]
#[derive(Clone, Debug, serde::Serialize)]
pub struct LogicInstance<'a> {
    pub tag: crate::types::Digest,
    pub is_consumed: bool,
    pub root: crate::types::Digest,
    pub app_data: &'a crate::types::AppData,
}

/// Compute the journal digest for a single LogicInstance.
///
/// This is used in the non-aggregated path where each logic proof is
/// verified individually.
#[cfg(feature = "non-aggregated-proofs")]
pub fn compute_logic_journal_digest(instance: &LogicInstance) -> Result<[u8; 32], PAError> {
    use anchor_lang::solana_program::hash::Hasher;
    use crate::risc0_serde;
    use serde::Serialize;

    struct HasherWordWriter<'a> {
        hasher: &'a mut Hasher,
    }

    impl<'a> risc0_serde::WordWrite for HasherWordWriter<'a> {
        fn write_words(&mut self, words: &[u32]) -> risc0_serde::Result<()> {
            for word in words {
                self.hasher.hash(&word.to_le_bytes());
            }
            Ok(())
        }

        fn write_padded_bytes(&mut self, bytes: &[u8]) -> risc0_serde::Result<()> {
            let mut offset = 0usize;
            while offset + 4 <= bytes.len() {
                self.hasher.hash(&bytes[offset..offset + 4]);
                offset += 4;
            }
            if offset < bytes.len() {
                let mut last = [0u8; 4];
                last[..bytes.len() - offset].copy_from_slice(&bytes[offset..]);
                self.hasher.hash(&last);
            }
            Ok(())
        }
    }

    let mut hasher = Hasher::default();
    let mut writer = HasherWordWriter { hasher: &mut hasher };
    let mut serializer = risc0_serde::Serializer::new(&mut writer);

    instance
        .serialize(&mut serializer)
        .map_err(|_| PAError::InvalidTransactionData)?;

    Ok(hasher.result().to_bytes())
}

/// Compute the journal digest for a single ComplianceInstance.
///
/// This is used in the non-aggregated path where each compliance proof is
/// verified individually.
///
/// Takes raw instance bytes (from cu.instance) to ensure exact match with prover format.
/// This avoids any serialization format differences between bincode and risc0_zkvm::serde.
#[cfg(feature = "non-aggregated-proofs")]
pub fn compute_compliance_journal_digest(instance_bytes: &[u8]) -> Result<[u8; 32], PAError> {
    use anchor_lang::solana_program::hash::Hasher;
    use crate::risc0_serde::WordWrite;
    use crate::encoding::bytes_to_words;

    struct HasherWordWriter<'a> {
        hasher: &'a mut Hasher,
    }

    impl<'a> WordWrite for HasherWordWriter<'a> {
        fn write_words(&mut self, words: &[u32]) -> crate::risc0_serde::Result<()> {
            for word in words {
                self.hasher.hash(&word.to_le_bytes());
            }
            Ok(())
        }

        fn write_padded_bytes(&mut self, bytes: &[u8]) -> crate::risc0_serde::Result<()> {
            let mut offset = 0usize;
            while offset + 4 <= bytes.len() {
                self.hasher.hash(&bytes[offset..offset + 4]);
                offset += 4;
            }
            if offset < bytes.len() {
                let mut last = [0u8; 4];
                last[..bytes.len() - offset].copy_from_slice(&bytes[offset..]);
                self.hasher.hash(&last);
            }
            Ok(())
        }
    }

    // Use raw instance bytes from prover (not re-serialized)
    let words = bytes_to_words(instance_bytes);

    let mut hasher = Hasher::default();
    let mut writer = HasherWordWriter { hasher: &mut hasher };

    // Write words directly as in aggregated path
    writer.write_words(&words).map_err(|_| PAError::InvalidTransactionData)?;

    Ok(hasher.result().to_bytes())
}
