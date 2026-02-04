//! Core type definitions for the Solana Protocol Adapter.

use serde::{Deserialize, Serialize};

pub use anoma_rm_risc0::{Digest, DIGEST_WORDS};
pub use anoma_rm_risc0::compliance::ComplianceInstance;
pub use anoma_rm_risc0::logic_instance::{AppData, ExpirableBlob};
pub use anoma_rm_risc0::action::Action;
pub use anoma_rm_risc0::compliance_unit::ComplianceUnit;
pub use anoma_rm_risc0::logic_instance::LogicVerifierInputs;
pub use anoma_rm_risc0::transaction::{Delta, Transaction};

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
