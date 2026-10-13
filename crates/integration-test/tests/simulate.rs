//! `simulate_execute`, pa-evm's `simulateExecute`: the settlement's own code,
//! run in the runtime's simulation, ending in `Simulated` when the
//! transaction would execute and in the adapter's error otherwise.

use anoma_pa_solana_client::PaError;
use anoma_pa_solana_integration_test::envs::local::Environment as SolanaLocalEnv;
use anoma_pa_testkit::environment::ProtocolAdapter as _;
use anoma_pa_testkit::fixtures::trivial;
use anoma_pa_testkit::prove_actions;
use anoma_pa_testkit::transaction::Transaction;
use anyhow::Context;

/// A proven transaction of one trivial action, `seed` telling its resources
/// apart from other tests'.
async fn proven(env: &SolanaLocalEnv, seed: u8) -> anyhow::Result<Transaction> {
    let actions = trivial::build_many(1, seed).context("failed to build a trivial action")?;
    prove_actions(env, &actions).await
}

/// `transaction` before it is proven: its aggregation without a proof.
fn unproven(mut transaction: Transaction) -> anyhow::Result<Transaction> {
    transaction
        .as_arm_mut()
        .aggregation
        .as_mut()
        .context("the transaction is aggregated")?
        .proof = vec![];
    Ok(transaction)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_transaction_that_would_settle_simulates_and_nothing_lands() -> anyhow::Result<()> {
    let mut env = SolanaLocalEnv::setup_bare().await?;
    let tx = proven(&env, 101).await?;
    let root = env.protocol_adapter.latest_root().await?;

    let simulated = env.protocol_adapter.simulate(tx.clone(), false).await?;

    let units = simulated
        .map_err(|error| anyhow::anyhow!("the simulation fails with {error:?}, not Simulated"))?;
    println!("simulated settlement: {units} compute units");
    anyhow::ensure!(
        env.protocol_adapter.latest_root().await? == root,
        "the simulation moved the root"
    );
    // The simulation spent no nullifier: the transaction still settles.
    env.protocol_adapter.settled(tx).await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unproven_transaction_simulates_with_its_proof_verification_skipped()
-> anyhow::Result<()> {
    let env = SolanaLocalEnv::setup_bare().await?;
    let tx = unproven(proven(&env, 102).await?)?;

    let simulated = env.protocol_adapter.simulate(tx, true).await?;

    anyhow::ensure!(
        simulated.is_ok(),
        "the unproven transaction fails with {simulated:?}, not Simulated"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unproven_transaction_simulated_with_its_proof_verified_has_an_invalid_proof()
-> anyhow::Result<()> {
    let env = SolanaLocalEnv::setup_bare().await?;
    let tx = unproven(proven(&env, 103).await?)?;

    let simulated = env.protocol_adapter.simulate(tx, false).await?;

    anyhow::ensure!(
        simulated == Err(PaError::InvalidProof),
        "the unproven transaction's simulation ends with {simulated:?}, not InvalidProof"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_settled_transaction_simulated_again_fails_as_its_settlement_would() -> anyhow::Result<()>
{
    let mut env = SolanaLocalEnv::setup_bare().await?;
    let tx = proven(&env, 104).await?;
    env.protocol_adapter.settled(tx.clone()).await?;

    for skip in [false, true] {
        let simulated = env.protocol_adapter.simulate(tx.clone(), skip).await?;
        anyhow::ensure!(
            simulated == Err(PaError::PreExistingNullifier),
            "the settled transaction's simulation (skip {skip}) ends with {simulated:?}, not \
             PreExistingNullifier"
        );
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_paused_adapter_refuses_a_simulation() -> anyhow::Result<()> {
    let mut env = SolanaLocalEnv::setup_bare().await?;
    let tx = proven(&env, 105).await?;
    env.protocol_adapter.pause().await?;

    let simulated = env.protocol_adapter.simulate(tx, true).await?;

    anyhow::ensure!(
        simulated == Err(PaError::EnforcedPause),
        "the paused adapter's simulation ends with {simulated:?}, not EnforcedPause"
    );
    Ok(())
}
