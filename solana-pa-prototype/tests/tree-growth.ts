/**
 * Consecutive settlements grow the commitment tree, emit their events, and
 * retain a marker for every produced root.
 */
import { SYSVAR_CLOCK_PUBKEY } from "@solana/web3.js";
import { assert } from "chai";
import { RESULT_LT } from "../client/constants";
import { loadFixture, createdCommitmentsOf as commitmentsOf } from "./utils/fixtures";
import { assertFails } from "./utils/helpers";
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

  it("settles v2 fixture (appends one leaf)", async () => {
    const stateBefore = await program.account.paStateAccount.fetch(paState);
    const nextIndexBefore = stateBefore.nextIndex.toNumber();

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
    assert.deepEqual(outputBytes, Buffer.from([RESULT_LT]), "output should be RESULT_LT");
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
    const txEvents = (await cpiEventsOf(sig)).events.filter((e) => e.name === "transactionExecutedEvent");
    assert.deepEqual(txEvents[0].data.isConsumed, [true], "the transaction's one tag is a consumed resource");
  });
});
