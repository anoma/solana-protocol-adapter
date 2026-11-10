//! `Submission::estimate`, the counterpart of pa-evm's integrators running
//! `eth_estimateGas` on `execute` with the verifier's code overridden: the
//! settlement's own transactions, run in a local SVM on the runtime's
//! accounts as of one read, with the verifier accepting every proof.

use std::time::Instant;

use anoma_pa_solana_client::estimate::Estimate;
use anoma_pa_solana_client::generated::protocol_adapter::error::ProtocolAdapterError;
use anoma_pa_solana_client::submission::Failure;
use anoma_pa_solana_integration_test::envs::local::Environment as SolanaLocalEnv;
use anoma_pa_solana_integration_test::fixtures::oversized_passthrough;
use anoma_pa_testkit::environment::ProtocolAdapter as _;
use anoma_pa_testkit::fixtures::trivial;
use anoma_pa_testkit::prove_actions;
use anoma_pa_testkit::transaction::Transaction;
use anoma_rm_risc0::Digest;
use anyhow::Context;

/// A proven transaction of one trivial action, `seed` telling its
/// resources apart from other tests'.
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
        .context("the proven transaction has no aggregation")?
        .proof = vec![];
    Ok(transaction)
}

/// Estimates `transaction`, unproven, and settles it proven: the estimate is
/// executable, with the compute units the settlement then uses.
async fn estimate_then_settle(
    env: &mut SolanaLocalEnv,
    transaction: Transaction,
) -> anyhow::Result<()> {
    let submission = env
        .protocol_adapter
        .prepare(unproven(transaction.clone())?)
        .await?;

    let started = Instant::now();
    let estimate = env.protocol_adapter.estimate(&submission).await?;
    println!("estimate: {estimate:?}, in {:?}", started.elapsed());

    let Estimate::Executable {
        units_consumed: estimated,
    } = estimate
    else {
        anyhow::bail!("the estimate is {estimate:?}, not executable");
    };
    // The estimate spent no nullifier: the transaction still settles.
    let executed = env.protocol_adapter.settled(transaction).await?;
    let used = executed.compute_units;
    println!("settlement: {used} compute units, estimated {estimated}");
    anyhow::ensure!(
        estimated == used,
        "the estimate gives {estimated} compute units, the settlement used {used}"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unproven_transaction_estimates_as_its_settlement_executes() -> anyhow::Result<()> {
    let mut env = SolanaLocalEnv::setup_bare().await?;
    let tx = proven(&env, 101).await?;
    estimate_then_settle(&mut env, tx).await
}

/// A settlement too large for one transaction is estimated through its
/// upload, as it is sent.
#[tokio::test(flavor = "multi_thread")]
async fn an_uploaded_settlement_estimates_as_it_executes() -> anyhow::Result<()> {
    let mut env = SolanaLocalEnv::setup_bare().await?;
    let tx = prove_actions(&env, &[oversized_passthrough(102)?]).await?;
    let plan = env
        .protocol_adapter
        .prepare(tx.clone())
        .await?
        .plan_settlement(0, u64::MAX)?;
    anyhow::ensure!(
        plan.upload.is_some(),
        "the settlement fits one transaction: the test needs a larger settlement"
    );
    estimate_then_settle(&mut env, tx).await
}

/// The estimate plans against its own read, as `eth_estimateGas` runs on one
/// block: a settlement landing after the submission was prepared moves the
/// root its marker derives from, and the estimate still executes.
#[tokio::test(flavor = "multi_thread")]
async fn a_settlement_landing_after_the_submission_was_prepared_does_not_fail_the_estimate()
-> anyhow::Result<()> {
    let mut env = SolanaLocalEnv::setup_bare().await?;
    let (tx, other) = (proven(&env, 103).await?, proven(&env, 104).await?);
    let submission = env.protocol_adapter.prepare(unproven(tx)?).await?;
    env.protocol_adapter.settled(other).await?;
    anyhow::ensure!(
        env.protocol_adapter.latest_root().await? != Digest::from_bytes(submission.state().root),
        "the settlement after the submission was prepared did not move the root"
    );

    let estimate = env.protocol_adapter.estimate(&submission).await?;

    anyhow::ensure!(
        matches!(estimate, Estimate::Executable { .. }),
        "the estimate after another settlement is {estimate:?}, not executable"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_settled_transaction_estimated_again_fails_as_its_settlement_would() -> anyhow::Result<()>
{
    let mut env = SolanaLocalEnv::setup_bare().await?;
    let tx = proven(&env, 105).await?;
    env.protocol_adapter.settled(tx.clone()).await?;

    let submission = env.protocol_adapter.prepare(unproven(tx)?).await?;
    let estimate = env.protocol_adapter.estimate(&submission).await?;

    anyhow::ensure!(
        estimate == Estimate::Failed(Failure::Adapter(ProtocolAdapterError::PreExistingNullifier)),
        "the settled transaction's estimate is {estimate:?}, not PreExistingNullifier"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_paused_adapter_fails_the_estimate() -> anyhow::Result<()> {
    let mut env = SolanaLocalEnv::setup_bare().await?;
    let tx = proven(&env, 106).await?;
    env.protocol_adapter.pause().await?;

    let submission = env.protocol_adapter.prepare(unproven(tx)?).await?;
    let estimate = env.protocol_adapter.estimate(&submission).await?;

    anyhow::ensure!(
        estimate == Estimate::Failed(Failure::Adapter(ProtocolAdapterError::EnforcedPause)),
        "the paused adapter's estimate is {estimate:?}, not EnforcedPause"
    );
    Ok(())
}
