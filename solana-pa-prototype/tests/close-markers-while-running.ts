/**
 * Teardown refuses to close markers while the adapter runs. The before hook
 * settles the primary fixture, leaving markers to attempt.
 */
import { assert } from "chai";
import { closeMarkersBatch } from "./utils";
import {
  provider,
  program,
  paState,
  ensureAdapterInitialized,
  assertPAError,
  useAdapterSuite,
} from "./utils/adapterSuite";

describe("protocol-adapter (close_markers_batch requires stopped state)", () => {
  const { settleFixture } = useAdapterSuite();

  before(async () => {
    await ensureAdapterInitialized();
    await settleFixture("batch_groth16.json");
  });

  it("close_markers_batch fails when PA is not stopped", async () => {
    const state = await program.account.paStateAccount.fetch(paState);
    assert.deepEqual(state.lifecycle, { running: {} }, "PA should be Running at start of test");

    // The markers the before hook's settlement created
    const markers = await provider.connection.getProgramAccounts(program.programId, {
      filters: [{ dataSize: 0 }],
    });
    assert.ok(markers.length > 0, "Should have markers to close");

    try {
      await closeMarkersBatch(
        program,
        provider.wallet.publicKey,
        markers.map(({ pubkey }) => pubkey),
      ).rpc();
      assert.fail("close_markers_batch should fail when PA is not stopped");
    } catch (e: any) {
      assertPAError(e, "NotStopped");
    }
  });
});
