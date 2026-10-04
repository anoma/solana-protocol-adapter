//! pa-testkit's environments for the Solana protocol adapter: a surfpool
//! runtime with the adapter set up, a prover, and a [`ProtocolAdapter`] that
//! settles proven transactions the way every submitter does.
//!
//! [`ProtocolAdapter`]: anoma_pa_testkit::environment::ProtocolAdapter

pub mod commitment_tree;
pub mod envs;
pub mod executed;
pub mod forwarders;
pub mod kind_table;
pub mod state;
pub mod test_forwarder;
