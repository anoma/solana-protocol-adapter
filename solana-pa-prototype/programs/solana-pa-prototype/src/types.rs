//! Solana-specific type definitions for the Protocol Adapter.

use serde::{Deserialize, Serialize};

/// Solana-specific external call structure.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SolanaExternalCall {
    pub program_id: [u8; 32],
    pub instruction_data: Vec<u8>,
    /// Must be non-empty: Solana has no way to represent an explicit empty
    /// return, so decoding rejects an empty value with `EmptyExpectedOutput`
    /// (see `external_calls::decode_external_call`).
    pub expected_output: Vec<u8>,
    pub output_mode: OutputMode,
    /// Number of accounts in this call's segment (including the forwarder program account).
    /// Committed in the ZK proof, making segment boundaries unambiguous.
    pub num_accounts: u8,
}

/// How to read external call output.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum OutputMode {
    ReturnData,
}
