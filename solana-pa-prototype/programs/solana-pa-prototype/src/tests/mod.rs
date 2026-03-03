//! All tests for solana-pa-prototype.
//!
//! Auditors: exclude this entire directory from review.
//! Production code is in the parent src/ directory.

pub mod utils;

mod delta_tests;
mod encoding_tests;
mod error_tests;
mod external_calls_tests;
mod groth16_tests;
mod lib_tests;
mod merkle_tests;
mod nullifier_tests;
mod root_tests;
mod txdata_tests;
mod types_tests;

// Property-based tests
pub mod proptests;
