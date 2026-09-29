/**
 * STATE-03 part 1: the historical-root committer settles at leaf 1, over the
 * tree the before hook builds (the primary fixture at leaf 0).
 */
import { assert } from "chai";
import { loadFixture, createdCommitmentsOf as commitmentsOf } from "./utils";
import {
  program,
  paState,
  deriveNullifierAccounts,
  ensureAdapterInitialized,
  assertFixtureUnsettled,
  buildSettleRemainingAccounts,
  useAdapterSuite,
} from "./utils/adapterSuite";

// ── Historical-root marker with a real Merkle-inclusion proof ───────────────
// Every other settling fixture consumes an `is_ephemeral: true` resource: the compliance
// circuit reports its consumed_commitment_tree_root as an unconstrained
// `ephemeral_root` (here, always PADDING_LEAF), never a real Merkle path. Since
// PADDING_LEAF is accepted by `is_root_valid` unconditionally, before the
// marker lookup ever runs, none of those settlements exercise the
// historical-root-marker branch.
//
// `is_ephemeral` is itself part of the resource's commitment hash, so a
// resource created as `is_ephemeral: true` (as every other one is) can never
// later be consumed through a genuine Merkle path -- flipping the flag would
// change the commitment and no longer match the leaf actually recorded
// on-chain. Proving the marker mechanism with a real inclusion proof therefore
// requires a purpose-built "committer" transaction whose created resource is
// genuinely non-ephemeral.
//
// The committer's Merkle path is baked to leaf index 1, so it settles
// immediately after batch_groth16.json (leaf 0), which the before hook
// settles. The matching "consumer", which spends that leaf through the real
// path, is historical-root.ts.
describe("protocol-adapter (STATE-03 part 1: commit a non-ephemeral leaf)", () => {
  const { settleFixtureViaTxData, settleFixture } = useAdapterSuite();

  before(async () => {
    await ensureAdapterInitialized();
    await settleFixture("batch_groth16.json");
  });

  it("settles the historical-root committer (its created resource is genuinely non-ephemeral)", async () => {
    await assertFixtureUnsettled("batch_groth16_historical_root_committer.json");

    const committerFixture = loadFixture("batch_groth16_historical_root_committer.json");
    const payload = Buffer.from(committerFixture.tx_b64, "base64");
    const nullifierAccounts = deriveNullifierAccounts(committerFixture.consumed_nullifiers_b64);
    const remainingAccounts = buildSettleRemainingAccounts(nullifierAccounts);

    const nextIndexBefore = (await program.account.paStateAccount.fetch(paState)).nextIndex.toNumber();
    await settleFixtureViaTxData(payload, remainingAccounts, {
      createdCommitments: commitmentsOf(committerFixture),
    });
    const nextIndexAfter = (await program.account.paStateAccount.fetch(paState)).nextIndex.toNumber();
    assert.equal(nextIndexAfter, nextIndexBefore + 1);
  });
});
