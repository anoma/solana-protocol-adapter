//! pa-testkit's chain-agnostic suite against each environment, and the
//! checks only the Solana adapter's harness makes. Every settlement also
//! checks that the tree pa-testkit builds gives the root the adapter stores.

use anoma_pa_solana_integration_test::envs::local::Environment as SolanaLocalEnv;
use anoma_pa_testkit::assert::{Needle, expect_integration_panic};
use anoma_pa_testkit::fixtures::trivial;
use anoma_pa_testkit::{execute_tx, prove_actions};
use anyhow::Context;
use solana_signer::Signer;

mod local {
    use super::*;

    anoma_pa_testkit::suite_tests!(
        SolanaLocalEnv::setup_bare(),
        // The mock verifier refuses a seal that is not the claim's
        // (programs/mock-verifier).
        refusal = Needle::Static(
            "Error Code: ClaimDigestMismatch. Error Number: 6600. Error Message: mock seal claim \
             digest mismatch."
        ),
    );
}

#[cfg(feature = "e2e")]
mod e2e_test {
    use anoma_pa_solana_integration_test::envs::e2e::Environment as SolanaE2eEnv;

    anoma_pa_testkit::suite_tests!(SolanaE2eEnv::setup_bare());
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

    let payer = env.protocol_adapter.payer.pubkey();
    let rpc = env.protocol_adapter.rpc.clone();
    let before = rpc.get_balance(&payer).await?;
    expect_integration_panic(Needle::Static(
        "consumed commitment tree root not found in PA for action 1 consumed resource 0",
    ))(execute_tx(&mut env, tx).await)?;
    let after = rpc.get_balance(&payer).await?;
    anyhow::ensure!(
        before == after,
        "the payer paid {} lamports for a transaction that must not be sent",
        before - after
    );
    Ok(())
}
