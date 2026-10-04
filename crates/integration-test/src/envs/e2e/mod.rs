pub mod config;
mod setup;

pub use super::common::protocol_adapter::ProtocolAdapter;
pub use anoma_pa_testkit::commitment_tree::FrontierCommitmentTree as CommitmentTree;
pub use anoma_pa_testkit::transaction::Transaction;
pub use config::E2eConfig;

/// End-to-end environment: a surfpool runtime forking devnet, settling on the
/// adapter devnet runs with the state devnet holds, with proofs from the
/// proving queue.
pub type Environment =
    super::common::environment::Environment<anoma_pa_testkit::prover::QueueProver>;
