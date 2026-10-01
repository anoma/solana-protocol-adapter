/**
 * Consecutive settlements grow the commitment tree, emit their events, and
 * retain a marker for every produced root.
 */
import { SYSVAR_CLOCK_PUBKEY } from "@solana/web3.js";
import { assert } from "chai";
import { RESULT_LT } from "../client/constants";
import { loadFixture, createdCommitmentsOf as commitmentsOf } from "./utils/fixtures";
import { assertFails, transactionIdOf } from "./utils/helpers";
import { predictRootAfterAppend } from "./utils/merkle";
import {
  provider,
  program,
  paState,
  blockTimeForwarderId,
  deriveRootPda,
  deriveNullifierAccounts,
  assertFixtureUnsettled,
  buildSettleRemainingAccounts,
  cpiEventsOf,
  useAdapterSuite,
} from "./utils/adapterSuite";

describe("protocol-adapter (Tree growth and multi-settlement)", () => {
  const { settleFixtureViaTxData, settleUnsettledFixture } = useAdapterSuite();

  let v2TxSig: string;
  // The root the v2 settlement produced: the tree it found with v2's leaf.
  let v2Root: Buffer;

  it("settles v2 fixture (appends one leaf)", async () => {
    const stateBefore = await program.account.paStateAccount.fetch(paState);
    const nextIndexBefore = stateBefore.nextIndex.toNumber();
    v2Root = await predictRootAfterAppend(program, paState, commitmentsOf(loadFixture("batch_groth16_v2.json")));

    v2TxSig = await settleUnsettledFixture("batch_groth16_v2.json");

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

    // pa-evm's order: each resource's forwarder calls (after its nullifier or
    // commitment is recorded) and payload events, the action's
    // ActionExecuted, then the transaction's new root and TransactionExecuted.
    // The consumed resource carries the block-time forwarder call; neither
    // resource carries an event-emitted payload.
    assert.deepEqual(
      events.map((e) => e.name),
      ["forwarderCallExecutedEvent", "actionExecutedEvent", "commitmentTreeRootAddedEvent", "transactionExecutedEvent"],
      "the settlement's events follow pa-evm's order",
    );
    assert.deepEqual(
      Buffer.from(events[2].data.root),
      v2Root,
      "CommitmentTreeRootAdded carries the root the settlement produced",
    );

    // pa-evm's ActionExecuted carries the action's nullifiers and commitments
    // with their logic refs; TransactionExecuted carries the transaction id.
    const fixture = loadFixture("batch_groth16_v2.json");
    const [, action, , executed] = events;
    const hex = (values: number[][]) => values.map((v) => Buffer.from(v).toString("hex"));
    assert.deepEqual(
      hex(action.data.nullifiers),
      fixture.consumed_nullifiers_b64.map((b: string) => Buffer.from(b, "base64").toString("hex")),
      "ActionExecuted lists the action's nullifiers",
    );
    assert.deepEqual(
      hex(action.data.commitments),
      commitmentsOf(fixture).map((c) => c.toString("hex")),
      "ActionExecuted lists the action's commitments",
    );
    assert.lengthOf(action.data.consumedLogicRefs, 1);
    assert.lengthOf(action.data.createdLogicRefs, 1);
    assert.deepEqual(
      Buffer.from(executed.data.transactionId),
      transactionIdOf([action.data.actionTreeRoot]),
      "TransactionExecuted carries the keccak of the action tree roots",
    );

    const fwdEvents = events.filter((e) => e.name === "forwarderCallExecutedEvent");
    assert.isAtLeast(fwdEvents.length, 1, "Should emit forwarderCallExecutedEvent");
    assert.ok(fwdEvents[0].data.forwarder.equals(blockTimeForwarderId), `forwarder should be ${blockTimeForwarderId}`);
    const outputBytes = Buffer.from(fwdEvents[0].data.output);
    assert.deepEqual(outputBytes, Buffer.from([RESULT_LT]), "output should be RESULT_LT");
  });

  it("retains a root marker for the v2 settlement's resulting root", async () => {
    // `newRootMarker` is a required named account, so the v2 settlement above
    // could not have succeeded without supplying it. This checks that one
    // instance, for the root the v2 settlement produced.
    const info = await provider.connection.getAccountInfo(deriveRootPda(v2Root));
    assert.ok(info, "Root marker should exist for the root the v2 settlement produced");
    assert.ok(info!.owner.equals(program.programId), "Root marker should be owned by the PA program");
  });

  // A settlement that appends commitments must retain its resulting root.
  it("rejects a settlement that creates resources but passes no root marker", async () => {
    const fixture = loadFixture("batch_groth16_v3.json");
    await assertFails(
      settleFixtureViaTxData(
        Buffer.from(fixture.tx_b64, "base64"),
        buildSettleRemainingAccounts(deriveNullifierAccounts(fixture.consumed_nullifiers_b64)),
        { newRootMarker: null },
      ),
      { program, error: "RootPdaMismatch" },
    );
  });

  it("settles v3 fixture (appends one leaf)", async () => {
    const stateBefore = await program.account.paStateAccount.fetch(paState);
    const nextIndexBefore = stateBefore.nextIndex.toNumber();

    await settleUnsettledFixture("batch_groth16_v3.json");

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

  // A settlement that creates nothing produces no root, so no marker account
  // belongs to it.
  it("rejects a root marker passed with a settlement that creates nothing", async () => {
    const fixture = loadFixture("batch_groth16_consume_only.json");
    const { root } = await program.account.paStateAccount.fetch(paState);
    await assertFails(
      settleFixtureViaTxData(
        Buffer.from(fixture.tx_b64, "base64"),
        buildSettleRemainingAccounts(deriveNullifierAccounts(fixture.consumed_nullifiers_b64)),
        { newRootMarker: deriveRootPda(Buffer.from(root)) },
      ),
      { program, error: "RootPdaMismatch" },
    );
  });

  // A transaction that creates no resource appends nothing, so the tree and
  // its latest root stay as they are and no root is recorded, as the EVM
  // adapter skips adding a root when none was produced. Settled after the
  // tree has grown, so the unchanged root is not the empty tree's.
  it("settles a transaction that creates nothing, leaving the tree and its roots untouched", async () => {
    const before = await program.account.paStateAccount.fetch(paState);
    assert.isAbove(before.nextIndex.toNumber(), 0, "the tree has grown");

    const sig = await settleUnsettledFixture("batch_groth16_consume_only.json");

    const after = await program.account.paStateAccount.fetch(paState);
    assert.equal(after.nextIndex.toNumber(), before.nextIndex.toNumber(), "no leaf is appended");
    assert.deepEqual(after.root, before.root, "the latest root is unchanged");

    const fixture = loadFixture("batch_groth16_consume_only.json");
    const [nullifierMarker] = deriveNullifierAccounts(fixture.consumed_nullifiers_b64);
    assert.isNotNull(
      await provider.connection.getAccountInfo(nullifierMarker.pubkey),
      "the consumed resource's nullifier is recorded",
    );
    const { events } = await cpiEventsOf(sig);
    const [action] = events.filter((e) => e.name === "actionExecutedEvent");
    assert.lengthOf(action.data.nullifiers, 1, "the action consumes one resource");
    assert.lengthOf(action.data.commitments, 0, "and creates none");
    assert.notInclude(
      events.map((e) => e.name),
      "commitmentTreeRootAddedEvent",
      "no root is added, as pa-evm adds none when a transaction creates nothing",
    );
  });
});
