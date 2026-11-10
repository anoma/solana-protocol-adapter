//! `Submission::estimate`, the counterpart of pa-evm's integrators running
//! `eth_estimateGas` on `execute` with the verifier's code overridden: the
//! settlement's own transactions, run in a local SVM on the runtime's
//! accounts as of one read, with the verifier accepting every proof.

use std::time::Instant;

use anoma_pa_solana_client::estimate::Estimate;
use anoma_pa_solana_client::generated::protocol_adapter::error::ProtocolAdapterError;
use anoma_pa_solana_client::submission::Failure;
use anoma_pa_solana_integration_test::envs::local::Environment as SolanaLocalEnv;
use anoma_pa_solana_integration_test::executed::Executed;
use anoma_pa_testkit::environment::ProtocolAdapter as _;
use anoma_pa_testkit::fixtures::{passthrough, trivial};
use anoma_pa_testkit::prove_actions;
use anoma_pa_testkit::transaction::Transaction;
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
        .context("the transaction is aggregated")?
        .proof = vec![];
    Ok(transaction)
}

/// The compute units a transaction used: the sum of what its top-level
/// instructions consumed, from the runtime's log.
fn units_consumed(executed: &Executed) -> anyhow::Result<u64> {
    let mut depth = 0;
    let mut units = 0;
    for line in &executed.logs {
        if line.ends_with(" invoke [1]") {
            depth = 1;
        } else if line.contains(" invoke [") {
            depth += 1;
        } else if let Some(consumed) = line.split(" consumed ").nth(1) {
            if depth == 1 {
                units += consumed
                    .split(' ')
                    .next()
                    .context("a consumed line names its units")?
                    .parse::<u64>()?;
            }
        } else if line.ends_with(" success") || line.contains(" failed: ") {
            depth -= 1;
        }
    }
    Ok(units)
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
    let root = env.protocol_adapter.latest_root().await?;

    let started = Instant::now();
    let estimate = env.protocol_adapter.estimate(&submission).await?;
    println!("estimate: {estimate:?}, in {:?}", started.elapsed());

    let Estimate::Executable {
        units_consumed: estimated,
    } = estimate
    else {
        anyhow::bail!("the estimate is {estimate:?}, not executable");
    };
    anyhow::ensure!(
        env.protocol_adapter.latest_root().await? == root,
        "the estimate moved the root"
    );
    // The estimate spent no nullifier: the transaction still settles.
    let executed = env.protocol_adapter.settled(transaction).await?;
    let used = units_consumed(&executed)?;
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
    // One pass-through action carrying a 4,800-byte application payload:
    // large in bytes, light in compute.
    let action = passthrough::build(
        102,
        vec![],
        passthrough::Overrides {
            application_payload: vec![vec![0x5a5a_5a5a; 1200]],
            ..passthrough::Overrides::default()
        },
    )
    .context("failed to build the pass-through action")?;
    let tx = prove_actions(&env, &[action.witnesses]).await?;
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
