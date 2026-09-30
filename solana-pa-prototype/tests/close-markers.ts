/**
 * close_markers_batch on a stopped adapter. The before hook settles the
 * primary fixture, leaving markers to close, then stops the adapter.
 */
import { Keypair, LAMPORTS_PER_SOL } from "@solana/web3.js";
import { assert } from "chai";
import { closeAllMarkers, closeMarkersBatch } from "../client/devTeardown";
import { assertFails } from "./utils/helpers";
import { provider, program, ensureAdapterInitialized, stopAdapter, useAdapterSuite } from "./utils/adapterSuite";

// ── Close instruction tests ──────────────────────────────────────────────

describe("protocol-adapter (Close instructions)", () => {
  const { funder, settleFixture } = useAdapterSuite();

  before(async () => {
    await ensureAdapterInitialized();
    await settleFixture("batch_groth16.json");
    await stopAdapter();
  });

  it("close_markers_batch closes marker PDAs and refunds rent", async () => {
    const markersBefore = (
      await provider.connection.getProgramAccounts(program.programId, {
        filters: [{ dataSize: 0 }],
      })
    ).length;
    assert.isAbove(markersBefore, 0, "the before hook's settlement leaves markers to close");
    const balanceBefore = await provider.connection.getBalance(provider.wallet.publicKey);

    assert.equal(await closeAllMarkers(program, provider.wallet.publicKey), markersBefore, "every marker is closed");

    // Verify markers are gone
    const markersAfter = await provider.connection.getProgramAccounts(program.programId, {
      filters: [{ dataSize: 0 }],
    });
    assert.equal(markersAfter.length, 0, "All markers should be closed");

    const balanceAfter = await provider.connection.getBalance(provider.wallet.publicKey);
    assert.ok(balanceAfter > balanceBefore, "Authority should have received rent refund");
    console.log(
      `    Closed ${markersBefore} markers, recovered ${((balanceAfter - balanceBefore) / LAMPORTS_PER_SOL).toFixed(6)} SOL`,
    );
  });

  it("close_markers_batch rejects non-authority", async () => {
    const fakeAuthority = Keypair.generate();
    await funder.fund(fakeAuthority, 1);

    await assertFails(closeMarkersBatch(program, fakeAuthority.publicKey, []).signers([fakeAuthority]).rpc(), {
      program,
      error: "Unauthorized",
    });
  });
});
