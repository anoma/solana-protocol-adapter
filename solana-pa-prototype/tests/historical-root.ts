/**
 * STATE-03 part 2: the consumer spends the committer's leaf through a root
 * that is no longer current. The before hook builds the tree the pair is
 * proven over (the primary fixture, then the committer at leaf 1) and
 * settles one more fixture, so the committer's root is historical.
 */
import { PublicKey } from "@solana/web3.js";
import { assert } from "chai";
import {
  EMPTY_TREE_ROOT_INITIAL,
  loadFixture,
  createdCommitmentsOf as commitmentsOf,
} from "./utils";
import {
  provider,
  program,
  paState,
  deriveRootPda,
  DUMMY_ROOT_MARKER,
  deriveNullifierAccounts,
  ensureAdapterInitialized,
  assertFixtureUnsettled,
  buildSettleRemainingAccounts,
  assertPAError,
  useAdapterSuite,
} from "./utils/adapterSuite";

// ── STATE-03 part 2: spend the committed leaf via its retained root ────────
// The before hook settles one more fixture after the committer, advancing the
// commitment tree past the root the committer produced, so that root is
// genuinely historical: it is
// neither the current root nor PADDING_LEAF. The only branch of
// `is_root_valid` that can still admit it is the root-marker lookup, which is
// exactly the branch no maintained test had ever exercised on a validator.
describe("protocol-adapter (STATE-03 part 2: settle against a retained historical root)", () => {
  const { settleFixtureViaTxData, settleFixture } = useAdapterSuite();

  before(async () => {
    await ensureAdapterInitialized();
    await settleFixture("batch_groth16.json");
    await settleFixture("batch_groth16_historical_root_committer.json");
    await settleFixture("batch_groth16_v2.json");
  });

  // The consumer's root must be genuinely superseded before either of the two
  // tests below runs, otherwise `is_root_valid` would return true on its
  // *first* branch (root == current root) and the marker branch would again go
  // untested. Asserted explicitly rather than assumed from test ordering.
  function consumerHistoricalRoot(): { rootB64: string; marker: PublicKey } {
    const consumerFixture = loadFixture("batch_groth16_historical_root.json");
    const historicalRoots = consumerFixture.historical_roots_b64 ?? [];
    assert.lengthOf(
      historicalRoots,
      1,
      "consumer fixture must carry exactly one historical (non-current) root",
    );
    const rootB64 = historicalRoots[0];

    // The exact trap every other fixture falls into: if the claimed root were
    // PADDING_LEAF, is_root_valid would accept it unconditionally, before the
    // marker lookup ever runs, and these tests would prove nothing about root
    // retention.
    assert.notEqual(
      rootB64,
      EMPTY_TREE_ROOT_INITIAL.toString("base64"),
      "consumer's historical root must not be PADDING_LEAF -- otherwise is_root_valid " +
        "admits it unconditionally and this test would not exercise the marker path",
    );

    return { rootB64, marker: deriveRootPda(Buffer.from(rootB64, "base64")) };
  }

  async function assertRootIsHistoricalNotCurrent(rootB64: string) {
    const state = await program.account.paStateAccount.fetch(paState);
    const currentRootB64 = Buffer.from(state.root as number[]).toString("base64");
    assert.notEqual(
      rootB64,
      currentRootB64,
      "consumer's root is still the current root -- is_root_valid would accept it on its " +
        "first branch, so the marker path would remain untested",
    );
  }

  // Runs before the success case: at this point the consumer has never
  // settled, so a rejection here is unambiguous. Its root is neither current
  // nor PADDING_LEAF, so withholding the marker leaves is_root_valid no branch
  // that can admit it. This is the half that proves the marker is load-bearing
  // rather than some other path letting the transaction through.
  it("rejects the consumer when its historical root marker is withheld (NonExistingRoot)", async () => {
    const { rootB64 } = consumerHistoricalRoot();
    await assertRootIsHistoricalNotCurrent(rootB64);

    const consumerFixture = loadFixture("batch_groth16_historical_root.json");
    const payload = Buffer.from(consumerFixture.tx_b64, "base64");
    const nullifierAccounts = deriveNullifierAccounts(consumerFixture.consumed_nullifiers_b64);
    // Deliberately no additionalHistoricalRootMarkers.
    const remainingAccounts = buildSettleRemainingAccounts(nullifierAccounts);

    try {
      await settleFixtureViaTxData(payload, remainingAccounts, { newRootMarker: DUMMY_ROOT_MARKER });
      assert.fail("expected settle to fail with NonExistingRoot");
    } catch (e: any) {
      assertPAError(e, "NonExistingRoot");
    }
  });

  it("settles the consumer when its historical root marker is supplied (retention works)", async () => {
    const { rootB64, marker } = consumerHistoricalRoot();
    await assertRootIsHistoricalNotCurrent(rootB64);

    const markerInfo = await provider.connection.getAccountInfo(marker);
    assert.ok(markerInfo, "historical root marker should exist from the committer's settlement");
    assert.ok(
      markerInfo!.owner.equals(program.programId),
      "root marker should be owned by the PA program",
    );

    await assertFixtureUnsettled("batch_groth16_historical_root.json");

    const consumerFixture = loadFixture("batch_groth16_historical_root.json");
    const payload = Buffer.from(consumerFixture.tx_b64, "base64");
    const nullifierAccounts = deriveNullifierAccounts(consumerFixture.consumed_nullifiers_b64);
    const remainingAccounts = buildSettleRemainingAccounts(nullifierAccounts, {
      additionalHistoricalRootMarkers: [marker],
    });

    const nextIndexBefore = (await program.account.paStateAccount.fetch(paState)).nextIndex.toNumber();
    await settleFixtureViaTxData(payload, remainingAccounts, {
      createdCommitments: commitmentsOf(consumerFixture),
    });
    const nextIndexAfter = (await program.account.paStateAccount.fetch(paState)).nextIndex.toNumber();
    assert.equal(nextIndexAfter, nextIndexBefore + 1, "consumer settlement should append its commitment");
  });
});
