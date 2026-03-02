//! Core type definitions for the Solana Protocol Adapter.

use serde::{Deserialize, Serialize};

pub use arm_core::action::Action;
pub use arm_core::compliance::ComplianceInstance;
pub use arm_core::compliance_unit::ComplianceUnit;
pub use arm_core::delta_types::DeltaWitness;
pub use arm_core::logic_instance::LogicVerifierInputs;
pub use arm_core::logic_instance::{AppData, ExpirableBlob};
pub use arm_core::transaction::{Delta, Transaction};
pub use arm_core::{Digest, DIGEST_WORDS};

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
