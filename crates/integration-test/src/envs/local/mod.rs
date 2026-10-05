mod block_time_forwarder;
mod setup;

/// Integration test execution environment: an offline surfpool runtime with
/// the adapter deployed and initialized against the mock verifier, and
/// pa-testkit's local prover.
pub type Environment =
    super::common::environment::Environment<anoma_pa_testkit::prover::LocalProver>;
