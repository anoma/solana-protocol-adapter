/**
 * Consecutive settlements grow the commitment tree, emit their events, and
 * retain a marker for every produced root.
 */
import { SYSVAR_CLOCK_PUBKEY } from "@solana/web3.js";
import { assert } from "chai";
import { loadFixture, createdCommitmentsOf as commitmentsOf } from "./utils";
import {
  provider,
  program,
  paState,
  blockTimeForwarderId,
  deriveRootPda,
  deriveNullifierAccounts,
  ensureAdapterInitialized,
  assertFixtureUnsettled,
  buildSettleRemainingAccounts,
  cpiEventsOf,
  useAdapterSuite,
} from "./utils/adapterSuite";

describe("protocol-adapter (Tree growth and multi-settlement)", () => {
  const { settleFixtureViaTxData } = useAdapterSuite();

  before(async () => {
    await ensureAdapterInitialized();
  });

  let v2TxSig: string;

  it("settles v2 fixture (appends one leaf)", async () => {
    await assertFixtureUnsettled("batch_groth16_v2.json");

    const stateBefore = await program.account.paStateAccount.fetch(paState);
    const nextIndexBefore = stateBefore.nextIndex.toNumber();

    const v2Fixture = loadFixture("batch_groth16_v2.json");
    const payload = Buffer.from(v2Fixture.tx_b64, "base64");
    const nullifierAccounts = deriveNullifierAccounts(v2Fixture.consumed_nullifiers_b64);
    const remainingAccounts = buildSettleRemainingAccounts(nullifierAccounts);

    v2TxSig = await settleFixtureViaTxData(payload, remainingAccounts, {
      createdCommitments: commitmentsOf(v2Fixture),
    });

    const state = await program.account.paStateAccount.fetch(paState);
    assert.equal(state.nextIndex.toNumber(), nextIndexBefore + 1);
  });

  it("verifies events from v2 settlement", async () => {
    // v2TxSig is set by the preceding test. If it is missing that settlement
    // failed, which must surface as a failure here rather than a skip: a
    // skipped test reports as pending, so a regression would cost two tests
    // and show only one red.
    assert.ok(
      v2TxSig,
      "v2 settlement did not produce a transaction signature — the preceding " +
        "'settles v2 fixture' test must have failed",
    );

    const { events } = await cpiEventsOf(v2TxSig);

    const actionEvents = events.filter((e) => e.name === "actionExecutedEvent");
    assert.isAtLeast(actionEvents.length, 1, "Should emit actionExecutedEvent");
    assert.ok(
      Array.isArray(actionEvents[0].data.actionTreeRoot) && actionEvents[0].data.actionTreeRoot.length === 32,
      "action_tree_root should be 32 bytes",
    );
    assert.equal(actionEvents[0].data.actionTagCount, 2, "action_tag_count should be 2 (consumed + created)");

    const txEvents = events.filter((e) => e.name === "transactionExecutedEvent");
    assert.equal(txEvents.length, 1, "Should emit exactly one transactionExecutedEvent");
    assert.equal(txEvents[0].data.tags.length, 2, "Should have 2 tags");
    assert.equal(txEvents[0].data.logicRefs.length, 2, "Should have 2 logic_refs");
    // Instance order is consumed-then-created per action; is_consumed states
    // each tag's role explicitly (indexers must not infer it from position).
    assert.deepEqual(
      txEvents[0].data.isConsumed,
      [true, false],
      "is_consumed should mark the nullifier then the commitment",
    );

    const fwdEvents = events.filter((e) => e.name === "forwarderCallExecutedEvent");
    assert.isAtLeast(fwdEvents.length, 1, "Should emit forwarderCallExecutedEvent");
    assert.ok(fwdEvents[0].data.forwarder.equals(blockTimeForwarderId), `forwarder should be ${blockTimeForwarderId}`);
    const outputBytes = Buffer.from(fwdEvents[0].data.output);
    assert.deepEqual(outputBytes, Buffer.from([0x00]), "output should be RESULT_LT (0x00)");
  });

  it("retains a root marker for the v2 settlement's resulting root", async () => {
    // `newRootMarker` is a required named account, so the v2 settlement above
    // could not have succeeded without supplying it. This checks that one
    // instance, for the root the v2 settlement produced.
    const state = await program.account.paStateAccount.fetch(paState);
    const currentRoot = Buffer.from(state.root as number[]);
    const rootMarkerPda = deriveRootPda(currentRoot);

    const info = await provider.connection.getAccountInfo(rootMarkerPda);
    assert.ok(info, "Root marker should exist for the current root after settlement");
    assert.ok(info!.owner.equals(program.programId), "Root marker should be owned by the PA program");
  });

  it("settles v3 fixture (appends one leaf)", async () => {
    await assertFixtureUnsettled("batch_groth16_v3.json");

    const stateBefore = await program.account.paStateAccount.fetch(paState);
    const nextIndexBefore = stateBefore.nextIndex.toNumber();

    const v3Fixture = loadFixture("batch_groth16_v3.json");
    const payload = Buffer.from(v3Fixture.tx_b64, "base64");
    const nullifierAccounts = deriveNullifierAccounts(v3Fixture.consumed_nullifiers_b64);
    const remainingAccounts = buildSettleRemainingAccounts(nullifierAccounts);

    await settleFixtureViaTxData(payload, remainingAccounts, {
      createdCommitments: commitmentsOf(v3Fixture),
    });

    const state = await program.account.paStateAccount.fetch(paState);
    assert.equal(state.nextIndex.toNumber(), nextIndexBefore + 1);
  });

  it("settles multi-call fixture with two external calls (appends one leaf)", async () => {
    await assertFixtureUnsettled("batch_groth16_multi_call.json");

    const stateBefore = await program.account.paStateAccount.fetch(paState);
    const nextIndexBefore = stateBefore.nextIndex.toNumber();

    const multiFixture = loadFixture("batch_groth16_multi_call.json");
    const payload = Buffer.from(multiFixture.tx_b64, "base64");
    const nullifierAccounts = deriveNullifierAccounts(multiFixture.consumed_nullifiers_b64);

    // Two external call segments: [btf, clock, btf, clock]
    const remainingAccounts = [
      ...nullifierAccounts,
      { pubkey: blockTimeForwarderId, isWritable: false, isSigner: false },
      { pubkey: SYSVAR_CLOCK_PUBKEY, isWritable: false, isSigner: false },
      { pubkey: blockTimeForwarderId, isWritable: false, isSigner: false },
      { pubkey: SYSVAR_CLOCK_PUBKEY, isWritable: false, isSigner: false },
    ];

    await settleFixtureViaTxData(payload, remainingAccounts, {
      createdCommitments: commitmentsOf(multiFixture),
    });

    const state = await program.account.paStateAccount.fetch(paState);
    assert.equal(state.nextIndex.toNumber(), nextIndexBefore + 1);
  });
});
