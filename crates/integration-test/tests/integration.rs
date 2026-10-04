//! pa-testkit's chain-agnostic suite against each environment, and the
//! checks only the Solana adapter's harness makes.

#[cfg(feature = "e2e")]
use anoma_pa_solana_integration_test::envs::e2e::Environment as SolanaE2eEnv;
use anoma_pa_solana_integration_test::envs::local::Environment as SolanaLocalEnv;
use anoma_pa_testkit::assert::{Needle, expect_integration_panic};
use anoma_pa_testkit::environment::Environment;
use anoma_pa_testkit::fixtures::trivial;
use anoma_pa_testkit::transaction::Transaction;
use anoma_pa_testkit::{commitment_root, execute_tx, prove_actions, suite};
use anyhow::Context;
use rstest::rstest;

#[rstest]
#[case::local(SolanaLocalEnv::setup_bare())]
#[cfg_attr(feature = "e2e", case::e2e_test(SolanaE2eEnv::setup_bare()))]
#[tokio::test(flavor = "multi_thread")]
async fn settles_a_trivial_transaction<Env: Environment>(
    #[future(awt)]
    #[case]
    env: anyhow::Result<Env>,
) -> anyhow::Result<()> {
    suite::settles_a_trivial_transaction(&mut env.context("env setup failed")?).await
}

#[rstest]
#[case::local(SolanaLocalEnv::setup_bare())]
#[cfg_attr(feature = "e2e", case::e2e_test(SolanaE2eEnv::setup_bare()))]
#[tokio::test(flavor = "multi_thread")]
async fn settles_an_n_to_m_transaction<Env: Environment>(
    #[future(awt)]
    #[case]
    env: anyhow::Result<Env>,
) -> anyhow::Result<()> {
    suite::settles_an_n_to_m_transaction(&mut env.context("env setup failed")?).await
}

#[rstest]
#[case::local(SolanaLocalEnv::setup_bare())]
#[cfg_attr(feature = "e2e", case::e2e_test(SolanaE2eEnv::setup_bare()))]
#[tokio::test(flavor = "multi_thread")]
async fn settles_a_multi_action_transaction<Env: Environment>(
    #[future(awt)]
    #[case]
    env: anyhow::Result<Env>,
) -> anyhow::Result<()> {
    suite::settles_a_multi_action_transaction(&mut env.context("env setup failed")?).await
}

#[rstest]
#[case::local(SolanaLocalEnv::setup_bare())]
#[tokio::test(flavor = "multi_thread")]
async fn settles_consume_only_transactions_without_a_root_change<Env: Environment>(
    #[future(awt)]
    #[case]
    env: anyhow::Result<Env>,
) -> anyhow::Result<()> {
    suite::settles_consume_only_transactions_without_a_root_change(
        &mut env.context("env setup failed")?,
    )
    .await
}

#[rstest]
#[case::local(SolanaLocalEnv::setup_bare())]
#[tokio::test(flavor = "multi_thread")]
async fn proving_refuses_a_nonzero_quantity<Env: Environment>(
    #[future(awt)]
    #[case]
    env: anyhow::Result<Env>,
) -> anyhow::Result<()> {
    suite::proving_refuses_a_nonzero_quantity(&env.context("env setup failed")?).await
}

#[rstest]
#[case::local(SolanaLocalEnv::setup_bare())]
#[tokio::test(flavor = "multi_thread")]
async fn proving_refuses_a_non_ephemeral_consumed_resource<Env: Environment>(
    #[future(awt)]
    #[case]
    env: anyhow::Result<Env>,
) -> anyhow::Result<()> {
    suite::proving_refuses_a_non_ephemeral_consumed_resource(&env.context("env setup failed")?)
        .await
}

#[rstest]
#[case::local(
    SolanaLocalEnv::setup_bare(),
    // The mock verifier refuses a seal that is not the claim's.
    Needle::Static(MOCK_VERIFIER_REFUSAL)
)]
#[tokio::test(flavor = "multi_thread")]
async fn settlement_refuses_a_tampered_aggregation_seal<Env>(
    #[future(awt)]
    #[case]
    env: anyhow::Result<Env>,
    #[case] refusal: Needle,
) -> anyhow::Result<()>
where
    Env: Environment<Transaction = Transaction>,
{
    suite::settlement_refuses_a_tampered_aggregation_seal(
        &mut env.context("env setup failed")?,
        refusal,
    )
    .await
}

/// What the runtime reports when the mock verifier refuses a seal: its
/// `ClaimDigestMismatch` (programs/mock-verifier).
const MOCK_VERIFIER_REFUSAL: &str = "Error Code: ClaimDigestMismatch. Error Number: 6600. Error Message: mock seal claim digest mismatch.";

#[tokio::test(flavor = "multi_thread")]
async fn the_tree_gives_the_root_the_adapter_stores_after_each_settlement() -> anyhow::Result<()> {
    let mut env = SolanaLocalEnv::setup_bare().await?;
    // build_many(2, seed) takes seeds seed and seed + 1.
    for seed in [61, 63] {
        let actions = trivial::build_many(2, seed).context("failed to build trivial actions")?;
        let tx = prove_actions(&env, &actions).await?;
        execute_tx(&mut env, tx).await?;
        let stored = env.protocol_adapter.state().await?.root;
        anyhow::ensure!(
            commitment_root(&env)?.as_bytes() == stored,
            "after the settlement of seed {seed}, the tree's root is not the adapter's"
        );
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_root_the_adapter_does_not_store_fails_before_anything_is_sent() -> anyhow::Result<()> {
    let mut env = SolanaLocalEnv::setup_bare().await?;
    let actions = trivial::build_many(2, 71).context("failed to build trivial actions")?;
    let mut tx = prove_actions(&env, &actions).await?;
    tx.as_arm_mut()
        .aggregation
        .as_mut()
        .context("the transaction is aggregated")?
        .instance
        .actions[1]
        .consumed_publics[0]
        .commitment_tree_root = anoma_rm_risc0::Digest::from_bytes([7; 32]);

    let payer = env.protocol_adapter.payer.clone();
    let rpc = env.protocol_adapter.rpc.clone();
    let before = rpc
        .get_balance(&solana_signer::Signer::pubkey(&*payer))
        .await?;
    expect_integration_panic(Needle::Static(
        "consumed commitment tree root not found in PA for action 1 consumed resource 0",
    ))(execute_tx(&mut env, tx).await)?;
    let after = rpc
        .get_balance(&solana_signer::Signer::pubkey(&*payer))
        .await?;
    anyhow::ensure!(
        before == after,
        "the payer paid {} lamports for a transaction that must not be sent",
        before - after
    );
    Ok(())
}
