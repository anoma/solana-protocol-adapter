//! The client's adapter instance (`anoma_pa_solana_client::instance`), pa-evm's
//! `IProtocolAdapter` bindings on Solana, against a deployed adapter: each
//! read answers what the test set up, the settlement it builds is the one the
//! adapter settled, and an adapter never initialized has no state to answer
//! from.

use std::collections::{BTreeSet, HashSet};

use anoma_pa_solana_client::generated::protocol_adapter::events::{
    ActionExecuted, CommitmentTreeRootAdded,
};
use anoma_pa_solana_client::instance::{InstanceError, ProtocolAdapter};
use anoma_pa_solana_client::settlement_input::{settled_resources, settlement_input};
use anoma_pa_solana_client::{
    CommitmentTreeState, MOCK_SELECTOR, PADDING_LEAF, SettlementAccounts, derive_nullifier_pda,
    derive_pa_state_pda, derive_root_marker_pda, derive_verifier_router_pdas,
};
use anoma_pa_solana_integration_test::envs::local::Environment as SolanaLocalEnv;
use anoma_pa_solana_integration_test::executed::Executed;
use anoma_pa_testkit::commitment_tree::depth_at;
use anoma_pa_testkit::environment::{DeniedLogicRef, ProtocolAdapter as _};
use anoma_pa_testkit::fixtures::trivial;
use anoma_pa_testkit::prove_actions;
use anoma_rm_risc0::Digest;
use anyhow::Context;
use solana_instruction::{AccountMeta, Instruction};
use solana_keypair::Keypair;
use solana_signer::Signer;
use surfpool_sdk::Pubkey;

/// The verifier router the local environment initializes the adapter with:
/// devnet's, from the copy the repository commits.
const VERIFIER_ROUTER: &str = "BetEAE4npinksQBxvqUN1KkCVjYFJywWao45MSWtp5yg";
/// The kind tree root the owner sets before the settlement, which settles
/// under the empty kind table, accepted with any stored root.
const KIND_TREE_ROOT: [u8; 32] = [0x4b; 32];
/// The logic refs the owner denies, one for consumed resources and one for
/// created ones.
const DENIED_CONSUMED: [u8; 32] = [0xc1; 32];
const DENIED_CREATED: [u8; 32] = [0xc2; 32];

/// The first top-level instruction of `executed` to `program`, as the
/// instruction it compiled: its program, its accounts with the signer and
/// writable flags the message gives them, and its data.
fn sent_instruction(executed: &Executed, program: &Pubkey) -> anyhow::Result<Instruction> {
    let message = &executed.transaction.message;
    let keys = message.static_account_keys();
    let compiled = message
        .instructions()
        .iter()
        .find(|ix| keys.get(usize::from(ix.program_id_index)) == Some(program))
        .with_context(|| format!("the transaction has no instruction to {program}"))?;
    let accounts = compiled
        .accounts
        .iter()
        .map(|&index| {
            let index = usize::from(index);
            let pubkey = *keys
                .get(index)
                .with_context(|| format!("an account index {index} of {} keys", keys.len()))?;
            let writable =
                message.is_maybe_writable_with_reserved_addresses(index, None::<&HashSet<Pubkey>>);
            Ok(AccountMeta {
                pubkey,
                is_signer: message.is_signer(index),
                is_writable: writable,
            })
        })
        .collect::<anyhow::Result<_>>()?;
    Ok(Instruction {
        program_id: *program,
        accounts,
        data: compiled.data.clone(),
    })
}

/// The empty subtree roots of heights 0 to `depth`, from arm's padding leaf
/// and node hash.
fn empty_subtree_roots(depth: usize) -> Vec<[u8; 32]> {
    std::iter::successors(Some(anoma_rm_risc0::merkle_path::PADDING_LEAF), |below| {
        Some(anoma_rm_risc0::utils::hash_two(below, below))
    })
    .take(depth + 1)
    .map(<[u8; 32]>::from)
    .collect()
}

// The adapter is initialized by the payer, owned by it, against the router
// and the mock verifier's selector; the owner sets a kind tree root, denies
// one logic ref on each side and hands the ownership to a new key; one
// transaction settles. Every read of the instance answers that.
#[tokio::test(flavor = "multi_thread")]
async fn every_read_answers_what_the_adapter_was_set_up_with() -> anyhow::Result<()> {
    let mut env = SolanaLocalEnv::setup_bare().await?;
    let pa = env.protocol_adapter.program;
    let payer = env.protocol_adapter.payer()?.clone();
    let rpc = env.protocol_adapter.rpc.clone();
    let router: Pubkey = VERIFIER_ROUTER.parse()?;
    let instance = ProtocolAdapter::at(&rpc, pa);
    anyhow::ensure!(instance.program_id() == pa, "the instance is not at {pa}");

    env.protocol_adapter
        .set_kind_tree_root(Digest::from_bytes(KIND_TREE_ROOT))
        .await?;
    env.protocol_adapter
        .deny_logic_refs(&[
            DeniedLogicRef {
                logic_ref: Digest::from_bytes(DENIED_CONSUMED),
                consumed: true,
            },
            DeniedLogicRef {
                logic_ref: Digest::from_bytes(DENIED_CREATED),
                consumed: false,
            },
        ])
        .await?;
    let owner = Keypair::new().pubkey();
    env.send(
        &[instance.transfer_ownership_ix(&payer.pubkey(), &owner)],
        &[],
    )
    .await?;

    let actions = trivial::build_many(1, 121).context("failed to build a trivial action")?;
    let tx = prove_actions(&env, &actions).await?;
    let input = settlement_input(tx.clone().into_arm())?;
    let resources = settled_resources(tx.as_arm())?;
    let executed = env.protocol_adapter.settled(tx).await?;
    let [action] = &executed.events::<ActionExecuted>(&pa)?[..] else {
        anyhow::bail!("the settlement does not execute one action");
    };
    let [added] = &executed.events::<CommitmentTreeRootAdded>(&pa)?[..] else {
        anyhow::bail!("the settlement does not add one root");
    };
    println!("settled: {action:?}, {added:?}");

    // The settlement as the instance builds it, from the accounts the
    // adapter's `settle` takes: the state, the new root's marker, the
    // router's accounts for the selector and the verifier; then the
    // nullifiers' markers and the consumed roots' (the padding leaf has
    // none).
    let pa_state = derive_pa_state_pda(&pa).0;
    let (router_state, verifier_entry) = derive_verifier_router_pdas(&router, MOCK_SELECTOR);
    let mut remaining: Vec<AccountMeta> = resources
        .nullifiers
        .iter()
        .map(|nf| AccountMeta::new(derive_nullifier_pda(&pa, &pa_state, nf).0, false))
        .collect();
    let consumed_roots: BTreeSet<[u8; 32]> = resources
        .consumed_roots
        .iter()
        .copied()
        .filter(|root| *root != PADDING_LEAF)
        .collect();
    remaining.extend(consumed_roots.iter().map(|root| {
        AccountMeta::new_readonly(derive_root_marker_pda(&pa, &pa_state, root).0, false)
    }));
    let built = instance.settle_ix(
        &payer.pubkey(),
        &input,
        SettlementAccounts {
            pa_state,
            new_root_marker: Some(derive_root_marker_pda(&pa, &pa_state, &added.root).0),
            verifier_router_program: router,
            router: router_state,
            verifier_entry,
            verifier_program: env.protocol_adapter.verifier_program,
            remaining_accounts: remaining,
        },
    );
    let sent = sent_instruction(&executed, &pa)?;
    anyhow::ensure!(
        built == sent,
        "the instance builds the settlement {built:?}, the adapter settled {sent:?}"
    );

    let paused = instance.paused().await?;
    println!("paused: {paused}");
    anyhow::ensure!(
        !paused,
        "paused() is {paused}, the adapter was never paused"
    );

    let verifier_paused = instance.risc_zero_verifier_paused(&payer.pubkey()).await?;
    println!("risc_zero_verifier_paused: {verifier_paused}");
    anyhow::ensure!(
        !verifier_paused,
        "risc_zero_verifier_paused() is {verifier_paused}, the mock verifier's entry is not \
         estopped"
    );

    let read_router = instance.risc_zero_verifier_router().await?;
    println!("risc_zero_verifier_router: {read_router}");
    anyhow::ensure!(
        read_router == router,
        "risc_zero_verifier_router() is {read_router}, the adapter was initialized with {router}"
    );

    let selector = instance.risc_zero_verifier_selector().await?;
    println!("risc_zero_verifier_selector: {selector:02x?}");
    anyhow::ensure!(
        selector == MOCK_SELECTOR,
        "risc_zero_verifier_selector() is {selector:02x?}, not the mock selector {MOCK_SELECTOR:02x?}"
    );

    let count = instance.commitment_count().await?;
    println!("commitment_count: {count}");
    anyhow::ensure!(
        count == action.commitments.len() as u64,
        "commitment_count() is {count}, the one settlement created {}",
        action.commitments.len()
    );

    // The tree of the settled commitments, which are all the adapter holds.
    let expected_depth = depth_at(action.commitments.len());
    let depth = instance.commitment_tree_depth().await?;
    println!("commitment_tree_depth: {depth}");
    anyhow::ensure!(
        usize::from(depth) == expected_depth,
        "commitment_tree_depth() is {depth}, a tree of {count} leaves is {expected_depth} deep"
    );

    let capacity = instance.commitment_tree_capacity().await?;
    println!("commitment_tree_capacity: {capacity}");
    anyhow::ensure!(
        capacity == 1 << expected_depth,
        "commitment_tree_capacity() is {capacity}, a tree {expected_depth} deep holds {}",
        1u64 << expected_depth
    );

    let siblings = instance.commitment_tree_left_siblings().await?;
    let expected_siblings = CommitmentTreeState::over(&action.commitments)?.frontier;
    println!("commitment_tree_left_siblings: {siblings:02x?}");
    anyhow::ensure!(
        siblings == expected_siblings,
        "commitment_tree_left_siblings() is {siblings:02x?}, the tree of the settled \
         commitments has {expected_siblings:02x?}"
    );

    let empty_roots = instance.commitment_tree_empty_subtree_roots().await?;
    println!("commitment_tree_empty_subtree_roots: {empty_roots:02x?}");
    anyhow::ensure!(
        empty_roots == empty_subtree_roots(expected_depth),
        "commitment_tree_empty_subtree_roots() is {empty_roots:02x?}, not the {} empty \
         subtree roots of a tree {expected_depth} deep",
        expected_depth + 1
    );

    let latest = instance.latest_commitment_tree_root().await?;
    println!(
        "latest_commitment_tree_root: {}",
        Digest::from_bytes(latest)
    );
    let harness_root = env.protocol_adapter.commitment_tree.root();
    anyhow::ensure!(
        latest == added.root && Digest::from_bytes(latest) == harness_root,
        "latest_commitment_tree_root() is {}, the settlement added {} and the harness's tree \
         gives {harness_root}",
        Digest::from_bytes(latest),
        Digest::from_bytes(added.root)
    );

    let historical = instance
        .is_commitment_tree_root_historical(&added.root)
        .await?;
    let never_a_root = [0x99; 32];
    let not_historical = instance
        .is_commitment_tree_root_historical(&never_a_root)
        .await?;
    println!(
        "is_commitment_tree_root_historical: the added root {historical}, another {not_historical}"
    );
    anyhow::ensure!(
        historical && !not_historical,
        "is_commitment_tree_root_historical() is {historical} for the added root and \
         {not_historical} for a root the tree never had"
    );

    let [nullifier] = &action.nullifiers[..] else {
        anyhow::bail!(
            "the action consumed {} resources, not one",
            action.nullifiers.len()
        );
    };
    let contained = instance.is_nullifier_contained(nullifier).await?;
    let absent = instance.is_nullifier_contained(&[0x77; 32]).await?;
    println!("is_nullifier_contained: the spent one {contained}, another {absent}");
    anyhow::ensure!(
        contained && !absent,
        "is_nullifier_contained() is {contained} for the spent nullifier and {absent} for one \
         never spent"
    );

    let kind_tree_root = instance.get_kind_tree_root().await?;
    println!("get_kind_tree_root: {}", Digest::from_bytes(kind_tree_root));
    anyhow::ensure!(
        kind_tree_root == KIND_TREE_ROOT,
        "get_kind_tree_root() is {kind_tree_root:02x?}, the owner set {KIND_TREE_ROOT:02x?}"
    );

    // A Merkle tree without entries has the padding leaf as its root.
    let empty_kind_tree_root = Digest::from_bytes(instance.empty_kind_tree_root());
    let padding = anoma_rm_risc0::merkle_path::PADDING_LEAF;
    println!("empty_kind_tree_root: {empty_kind_tree_root}");
    anyhow::ensure!(
        empty_kind_tree_root == padding,
        "empty_kind_tree_root() is {empty_kind_tree_root}, the root of the empty kind table \
         is arm's padding leaf {padding}"
    );

    let denials = [
        (DENIED_CONSUMED, true, true),
        (DENIED_CONSUMED, false, false),
        (DENIED_CREATED, false, true),
        (DENIED_CREATED, true, false),
    ];
    for (logic_ref, consumed, expected) in denials {
        let denied = instance.is_logic_ref_denied(&logic_ref, consumed).await?;
        println!("is_logic_ref_denied({logic_ref:02x?}, consumed {consumed}): {denied}");
        anyhow::ensure!(
            denied == expected,
            "is_logic_ref_denied({logic_ref:02x?}, consumed {consumed}) is {denied}, not \
             {expected}"
        );
    }

    let read_owner = instance.owner().await?;
    println!("owner: {read_owner}");
    anyhow::ensure!(
        read_owner == owner,
        "owner() is {read_owner}, the ownership was transferred to {owner}"
    );
    Ok(())
}

// An adapter deployed but never initialized has no state account: every read
// of its state says so, naming the account, rather than decoding nothing; its
// marker reads answer false.
#[tokio::test(flavor = "multi_thread")]
async fn an_adapter_never_initialized_has_no_state_to_read() -> anyhow::Result<()> {
    let env = SolanaLocalEnv::setup_bare().await?;
    let payer = env.protocol_adapter.payer_pubkey();
    let program = Pubkey::new_unique();
    env.deploy_program(
        program,
        include_bytes!("../programs/protocol_adapter.so"),
        payer,
    )?;
    let rpc = env.protocol_adapter.rpc.clone();
    let instance = ProtocolAdapter::at(&rpc, program);
    let state = derive_pa_state_pda(&program).0;

    let missing = |read: &str, result: Result<String, InstanceError>| {
        println!("{read}: {result:?}");
        anyhow::ensure!(
            matches!(&result, Err(InstanceError::AccountMissing { address }) if *address == state),
            "{read} on an adapter never initialized is {result:?}, not AccountMissing of its \
             state {state}"
        );
        Ok(())
    };
    let shown = |value: &dyn std::fmt::Debug| format!("{value:?}");
    missing("paused", instance.paused().await.map(|v| shown(&v)))?;
    missing(
        "risc_zero_verifier_paused",
        instance
            .risc_zero_verifier_paused(&payer)
            .await
            .map(|v| shown(&v)),
    )?;
    missing(
        "risc_zero_verifier_router",
        instance
            .risc_zero_verifier_router()
            .await
            .map(|v| shown(&v)),
    )?;
    missing(
        "risc_zero_verifier_selector",
        instance
            .risc_zero_verifier_selector()
            .await
            .map(|v| shown(&v)),
    )?;
    missing(
        "commitment_count",
        instance.commitment_count().await.map(|v| shown(&v)),
    )?;
    missing(
        "commitment_tree_depth",
        instance.commitment_tree_depth().await.map(|v| shown(&v)),
    )?;
    missing(
        "commitment_tree_capacity",
        instance.commitment_tree_capacity().await.map(|v| shown(&v)),
    )?;
    missing(
        "commitment_tree_left_siblings",
        instance
            .commitment_tree_left_siblings()
            .await
            .map(|v| shown(&v)),
    )?;
    missing(
        "commitment_tree_empty_subtree_roots",
        instance
            .commitment_tree_empty_subtree_roots()
            .await
            .map(|v| shown(&v)),
    )?;
    missing(
        "latest_commitment_tree_root",
        instance
            .latest_commitment_tree_root()
            .await
            .map(|v| shown(&v)),
    )?;
    missing(
        "get_kind_tree_root",
        instance.get_kind_tree_root().await.map(|v| shown(&v)),
    )?;
    missing(
        "is_logic_ref_denied",
        instance
            .is_logic_ref_denied(&DENIED_CONSUMED, true)
            .await
            .map(|v| shown(&v)),
    )?;
    missing("owner", instance.owner().await.map(|v| shown(&v)))?;

    // The marker reads read no state: an adapter never initialized holds no
    // marker, as an EVM adapter's empty storage holds none.
    let contained = instance.is_nullifier_contained(&[1; 32]).await?;
    let historical = instance
        .is_commitment_tree_root_historical(&[1; 32])
        .await?;
    println!(
        "is_nullifier_contained: {contained}; is_commitment_tree_root_historical: {historical}"
    );
    anyhow::ensure!(
        !contained && !historical,
        "an adapter never initialized has a nullifier ({contained}) or a historical root \
         ({historical})"
    );
    Ok(())
}
