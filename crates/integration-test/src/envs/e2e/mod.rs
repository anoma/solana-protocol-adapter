mod config;
mod kind_table;
mod setup;

use anoma_pa_testkit::prover::{QueueProver, Risc0Prover};
use anoma_pa_testkit::transaction::Transaction;
use anoma_pa_testkit::witness::ActionWitnesses;

/// End-to-end environment: a surfpool runtime forking devnet, settling on the
/// adapter devnet runs with the state devnet holds, with real proofs.
pub type Environment = super::common::environment::Environment<Prover>;

/// Where the e2e environment's real proofs come from (`E2eConfig`).
pub enum Prover {
    /// The remote proving queue.
    Queue(QueueProver),
    /// risc0 on this machine; its Groth16 step needs a container runtime.
    Risc0(Risc0Prover),
}

impl anoma_pa_testkit::environment::Prover for Prover {
    async fn prove(&self, actions: &[ActionWitnesses]) -> anyhow::Result<Transaction> {
        match self {
            Self::Queue(prover) => prover.prove(actions).await,
            Self::Risc0(prover) => prover.prove(actions).await,
        }
    }
}
