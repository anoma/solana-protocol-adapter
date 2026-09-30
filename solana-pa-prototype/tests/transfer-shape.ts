/**
 * The three-action transfer-shape fixture: it exhausts the default heap and
 * settles within the extended one.
 */
import { Keypair } from "@solana/web3.js";
import { assert } from "chai";
import {
  loadFixture,
  createdCommitmentsOf as commitmentsOf,
} from "./utils";
import {
  program,
  paState,
  DUMMY_ROOT_MARKER,
  deriveNullifierAccounts,
  ensureAdapterInitialized,
  extractPAErrorCode,
  cpiEventsOf,
  settleFromTxDataBuilder,
  useAdapterSuite,
} from "./utils/adapterSuite";

describe("protocol-adapter (Multi-action transfer-shape settlement)", () => {
  const { funder, uploadTxData, settleFixtureViaTxData } = useAdapterSuite();

  before(async () => {
    await ensureAdapterInitialized();
  });

  // Successor of the imported-mainnet-transfer OOM regression: a synthetic
  // three-action transaction at least as large on the wire as the captured
  // production transfer (fixture-gen enforces the size), with event-emitted
  // payload blobs on every created resource. Settling it within the CU and
  // heap budgets is the regression being tested.

  // Adequacy guard: the original fixture existed because that transfer
  // could not settle in the default heap (the 256 KiB allocator and the
  // requestHeapFrame calls landed with it). A replacement only regression-
  // tests the OOM path if it, too, exhausts the default heap — so this must
  // FAIL without the heap frame. If it ever starts succeeding, the fixture
  // no longer stresses the heap and must grow. Runs before the successful
  // settlement so a surprise success cannot consume the nullifiers first.
  it("cannot settle the transfer-shape fixture without the extended heap budget", async () => {
    const fx = loadFixture("batch_groth16_transfer_shape.json");
    const payload = Buffer.from(fx.tx_b64, "base64");
    const authority = Keypair.generate();
    await funder.fund(authority, 2);
    const { uploadId, txData } = await uploadTxData(authority, payload);
    const nullifierAccounts = deriveNullifierAccounts(fx.consumed_nullifiers_b64);

    let caught: any = null;
    try {
      // Full CU budget but NO requestHeapFrame: only the default heap.
      await settleFromTxDataBuilder(authority.publicKey, uploadId, txData, DUMMY_ROOT_MARKER, nullifierAccounts, false)
        .signers([authority])
        .rpc();
    } catch (e: any) {
      caught = e;
    }
    assert.isNotNull(
      caught,
      "transfer-shape settlement succeeded in the default heap — the fixture no " +
        "longer exercises the OOM regression; increase its payload sizes",
    );

    // The failure must be genuine memory exhaustion, not a later check
    // (e.g. the dummy root marker) reached after the heap survived: a PA
    // error code would mean the program ran to a logic check, so the
    // fixture did NOT exhaust the default heap.
    const code = extractPAErrorCode(caught);
    const logs: string[] = caught?.logs ?? caught?.error?.logs ?? [];
    assert.isNull(
      code,
      `expected a runtime memory failure, got PA error code ${code} — the ` +
        "fixture settled past the heap in the default budget; increase its " +
        `payload sizes\nLogs:\n${logs.slice(-15).join("\n")}`,
    );
    assert.ok(
      logs.some((l) => /memory allocation failed|out of memory|Access violation/i.test(l)),
      `expected a memory-exhaustion log line\nLogs:\n${logs.slice(-15).join("\n")}`,
    );
  });

  it("settles the three-action transfer-shape fixture and emits payload events", async () => {
    const fx = loadFixture("batch_groth16_transfer_shape.json");
    const payload = Buffer.from(fx.tx_b64, "base64");
    assert.equal(fx.consumed_nullifiers_b64.length, 3, "fixture should have 3 actions");

    const stateBefore = await program.account.paStateAccount.fetch(paState);
    const nextIndexBefore = stateBefore.nextIndex.toNumber();

    const nullifierAccounts = deriveNullifierAccounts(fx.consumed_nullifiers_b64);
    const sig = await settleFixtureViaTxData(payload, nullifierAccounts, {
      createdCommitments: commitmentsOf(fx),
    });

    const stateAfter = await program.account.paStateAccount.fetch(paState);
    assert.equal(
      stateAfter.nextIndex.toNumber(),
      nextIndexBefore + 3,
      "three created commitments should be appended",
    );

    const { tx: txResult, events } = await cpiEventsOf(sig);
    const innerCount =
      txResult.meta?.innerInstructions?.reduce((n, g) => n + g.instructions.length, 0) ?? 0;
    console.log(
      `transfer-shape settlement: ${txResult!.meta?.computeUnitsConsumed} CU, ` +
        `${events.length} CPI events, ${innerCount} inner instructions of the ` +
        "64-instruction trace limit",
    );

    const actionEvents = events.filter((e) => e.name === "actionExecutedEvent");
    assert.equal(actionEvents.length, 3, "one actionExecutedEvent per action");
    for (const ev of actionEvents) {
      assert.equal(ev.data.actionTagCount, 2, "each action has one consumed + one created");
    }

    const txEvents = events.filter((e) => e.name === "transactionExecutedEvent");
    assert.equal(txEvents.length, 1);
    assert.equal(txEvents[0].data.tags.length, 6, "6 tags across 3 actions");
    assert.deepEqual(
      txEvents[0].data.isConsumed,
      [true, false, true, false, true, false],
      "consumed-then-created per action, in instance order",
    );

    // Each created resource carries one resource payload (512 words) and one
    // discovery payload (192 words) with deletion criterion "never", so both
    // are emitted with index 0 under the created resource's commitment tag.
    const resourceEvents = events.filter((e) => e.name === "resourcePayloadEvent");
    const discoveryEvents = events.filter((e) => e.name === "discoveryPayloadEvent");
    assert.equal(resourceEvents.length, 3, "one resource payload event per created resource");
    assert.equal(discoveryEvents.length, 3, "one discovery payload event per created resource");
    const createdTags = txEvents[0].data.tags.filter(
      (_: unknown, i: number) => !txEvents[0].data.isConsumed[i],
    );
    for (const [evs, byteLen] of [
      [resourceEvents, 2048],
      [discoveryEvents, 768],
    ] as const) {
      for (const ev of evs) {
        assert.equal(ev.data.index, 0);
        assert.equal(Buffer.from(ev.data.blob).length, byteLen);
        assert.ok(
          createdTags.some((t: number[]) => Buffer.from(t).equals(Buffer.from(ev.data.tag))),
          "payload event tag should be a created commitment",
        );
      }
    }
  });
});
