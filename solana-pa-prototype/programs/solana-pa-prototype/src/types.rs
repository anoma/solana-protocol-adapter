//! Solana-specific type definitions for the Protocol Adapter.
//!
//! These types are owned by `anoma-pa-solana-client` and re-exported here so
//! existing imports inside the PA crate (`crate::types::SolanaExternalCall`)
//! continue to compile without rewriting every call site. The canonical
//! definitions and bincode wire-format guarantees live in the client crate.

pub use anoma_pa_solana_client::external_call::{OutputMode, SolanaExternalCall};
