mod setup;

pub use super::common::protocol_adapter::ProtocolAdapter;
pub use anoma_pa_testkit::commitment_tree::FrontierCommitmentTree as CommitmentTree;
pub use anoma_pa_testkit::transaction::Transaction;

/// Integration test execution environment: an offline surfpool runtime with
/// the adapter deployed and initialized against the mock verifier, and
/// pa-testkit's local prover.
pub type Environment =
    super::common::environment::Environment<anoma_pa_testkit::prover::LocalProver>;
