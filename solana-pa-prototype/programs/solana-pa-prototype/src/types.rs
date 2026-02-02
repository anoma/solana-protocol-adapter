//! Core type definitions for the Solana Protocol Adapter.
//!
//! Shared types are re-exported from arm-risc0. Transaction-related types that
//! require zkvm are defined locally to maintain serialization compatibility.

use serde::{Deserialize, Serialize};

pub use anoma_rm_risc0::{Digest, DIGEST_WORDS};
pub use anoma_rm_risc0::compliance::ComplianceInstance;
pub use anoma_rm_risc0::logic_instance::{AppData, ExpirableBlob};

// Transaction-related types (local definitions for zkvm-free builds)

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LogicVerifierInputs {
    pub tag: Digest,
    pub verifying_key: Digest,
    pub app_data: AppData,
    pub proof: Option<Vec<u8>>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ComplianceUnit {
    pub proof: Option<Vec<u8>>,
    pub instance: ComplianceInstance,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Action {
    pub compliance_units: Vec<ComplianceUnit>,
    pub logic_verifier_inputs: Vec<LogicVerifierInputs>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Delta {
    Witness(Vec<u8>),
    Proof(Vec<u8>),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Transaction {
    pub actions: Vec<Action>,
    pub delta_proof: Delta,
    pub expected_balance: Option<Vec<u8>>,
    pub aggregation_proof: Option<Vec<u8>>,
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
