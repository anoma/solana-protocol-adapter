//! Property-based tests for Solana Protocol Adapter
//!
//! These tests use proptest to verify invariants across randomized inputs,
//! providing fuzzing parity with the EVM Protocol Adapter's Foundry tests.

pub mod strategies;

pub mod delta_props;
pub mod encoding_props;
pub mod external_calls_props;
pub mod merkle_props;
pub mod serialization_props;
pub mod settle_props;
