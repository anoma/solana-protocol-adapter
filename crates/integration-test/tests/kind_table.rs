//! The kind table the adapter checks proofs against. The prover's kind table
//! is process-wide, so this file, a process of its own, loads solana-devnet's.

use anoma_pa_solana_client::set_kind_table_commitment_ix;
use anoma_pa_solana_integration_test::envs::local::Environment as SolanaLocalEnv;
use anoma_pa_solana_integration_test::kind_table;
use anoma_pa_testkit::assert::{Needle, expect_integration_panic};
use anoma_pa_testkit::fixtures::passthrough;
use anoma_pa_testkit::transaction::Transaction;
use anoma_pa_testkit::{execute_tx, prove_actions};
use solana_signer::Signer;

// A transaction proven against a kind table the adapter does not hold is
// refused; once the authority installs that table's commitment, it settles.
#[tokio::test(flavor = "multi_thread")]
async fn settles_a_transaction_proven_against_the_kind_table_the_authority_installs()
-> anyhow::Result<()> {
    let devnet = kind_table::load_devnet()?;
    let mut env = SolanaLocalEnv::setup_bare().await?;
    let held = env.protocol_adapter.state().await?.kind_table_commitment;
    anyhow::ensure!(
        held != devnet,
        "the adapter already holds solana-devnet's kind table"
    );

    let action = passthrough::build(1, Vec::new(), passthrough::Overrides::default())?;
    let tx = prove_actions(&env, &[action.witnesses]).await?;
    expect_integration_panic(Needle::Static("Error Code: KindTableCommitmentMismatch."))(
        execute_tx(&mut env, Transaction::from_arm(tx.as_arm().clone())).await,
    )?;

    let pa = env.protocol_adapter.program;
    let authority = env.protocol_adapter.payer.pubkey();
    env.send(
        &[set_kind_table_commitment_ix(&pa, &authority, devnet)],
        &[],
    )
    .await?;
    anyhow::ensure!(
        env.protocol_adapter.state().await?.kind_table_commitment == devnet,
        "the adapter does not hold the commitment the authority installed"
    );
    execute_tx(&mut env, tx).await
}
