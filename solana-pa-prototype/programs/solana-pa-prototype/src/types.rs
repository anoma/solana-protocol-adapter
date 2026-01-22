//! Core type definitions for the Solana Protocol Adapter.
//!
//! These types mirror arm-risc0 structures for deserialization compatibility.

use serde::{Deserialize, Serialize};

/// Number of u32 words in a Digest.
pub const DIGEST_WORDS: usize = 8;

/// A 32-byte digest, equivalent to risc0_zkvm::sha::Digest.
/// Uses native endianness for word storage (matches risc0).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Digest(pub [u32; DIGEST_WORDS]);

impl Digest {
    /// Create a Digest from a hex string (used for known constants).
    pub fn from_hex(hex: &str) -> Self {
        let bytes = hex::decode(hex).expect("invalid hex");
        let mut arr = [0u8; 32];
        arr.copy_from_slice(&bytes);
        Self::from_bytes(arr)
    }

    /// Create from 32 bytes. Uses native word order (bytemuck cast).
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        let words: &[u32; 8] = bytemuck::cast_ref(&bytes);
        Digest(*words)
    }

    /// Convert to 32 bytes. Uses native word order (bytemuck cast).
    pub fn to_bytes(&self) -> [u8; 32] {
        *bytemuck::cast_ref(&self.0)
    }

    pub fn as_words(&self) -> &[u32; DIGEST_WORDS] {
        &self.0
    }
}

/// Compliance instance from arm-risc0 proving_system.
/// This is the public output of a compliance circuit, parsed from ComplianceUnit.instance.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ComplianceInstance {
    pub consumed_nullifier: Digest,
    pub consumed_logic_ref: Digest,
    pub consumed_commitment_tree_root: Digest,
    pub created_commitment: Digest,
    pub created_logic_ref: Digest,
    pub delta_x: [u32; 8],
    pub delta_y: [u32; 8],
}

/// Expirable blob from arm-risc0.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ExpirableBlob {
    pub blob: Vec<u32>,
    pub deletion_criterion: u32,
}

/// Application data attached to logic verifier inputs.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct AppData {
    pub resource_payload: Vec<ExpirableBlob>,
    pub discovery_payload: Vec<ExpirableBlob>,
    pub external_payload: Vec<ExpirableBlob>,
    pub application_payload: Vec<ExpirableBlob>,
}

/// Logic verifier inputs from arm-risc0.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LogicVerifierInputs {
    pub tag: Digest,
    pub verifying_key: Digest,
    pub app_data: AppData,
    pub proof: Option<Vec<u8>>,
}

/// Compliance unit from arm-risc0.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ComplianceUnit {
    pub proof: Option<Vec<u8>>,
    pub instance: ComplianceInstance,
}

/// Action containing compliance units and logic verifier inputs.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Action {
    pub compliance_units: Vec<ComplianceUnit>,
    pub logic_verifier_inputs: Vec<LogicVerifierInputs>,
}

/// Delta proof variants.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Delta {
    /// Matches `arm-risc0`'s `DeltaWitness` serialization: bincode "bytes" (len + raw bytes).
    Witness(Vec<u8>),
    /// Matches `arm-risc0`'s `DeltaProof` serialization: bincode "bytes" (len + raw bytes).
    Proof(Vec<u8>),
}

/// RM Transaction structure matching arm-risc0.
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
