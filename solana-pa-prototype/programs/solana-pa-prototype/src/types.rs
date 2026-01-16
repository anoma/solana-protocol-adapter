//! Core type definitions for the Solana Protocol Adapter.
//!
//! These types mirror arm-risc0 structures for deserialization compatibility.

use arm_types::utils::Digest;
use serde::{Deserialize, Serialize};

pub trait ToBytes {
    fn to_bytes(&self) -> [u8; 32];
}

impl ToBytes for Digest {
    /// Convert to 32 bytes. Uses native word order (bytemuck cast).
    fn to_bytes(&self) -> [u8; 32] {
        self.as_bytes().try_into().unwrap()
    }
}

/// Solana-specific external call structure.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SolanaExternalCall {
    pub program_id: [u8; 32],
    pub instruction_data: Vec<u8>,
    pub expected_output: Vec<u8>,
    pub output_mode: OutputMode,
}

/// How to read external call output.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum OutputMode {
    ReturnData,
    OutputAccount { index: u8, offset: u32, len: u32 },
}
