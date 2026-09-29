/**
 * The kind-table commitment setter. The before hook settles the primary
 * fixture, so a re-submission of it reaches nullifier creation exactly when
 * the stored commitment matches its instance.
 */
import { Keypair } from "@solana/web3.js";
import { assert } from "chai";
import {
  EMPTY_KIND_TABLE_COMMITMENT,
  AUTHORITY_MISMATCH_PATTERN,
  randomRef,
  assertRejects,
  setKindTableCommitment,
} from "./utils";
import {
  provider,
  program,
  paState,
  fixture,
  DUMMY_ROOT_MARKER,
  deriveNullifierAccounts,
  ensureAdapterInitialized,
  buildSettleRemainingAccounts,
  cpiEventsOf,
  settleFromTxDataBuilder,
  useAdapterSuite,
} from "./utils/adapterSuite";

describe("protocol-adapter (kind table commitment)", () => {
  const { funder, uploadTxData, settleFixture } = useAdapterSuite();

  before(async () => {
    await ensureAdapterInitialized();
    await settleFixture("batch_groth16.json");
  });

  // Mirrors pa-evm ProtocolAdapter.setKindTableCommitment: owner-only, zero
  // rejected, KindTableCommitmentUpdated emitted. The stored commitment is
  // what every settled aggregation instance must carry, so a change rejects
  // transactions proven against the previous table until it is changed back.
  const empty = Array.from(EMPTY_KIND_TABLE_COMMITMENT);

  // A fresh upload of the already-settled primary fixture. Under another
  // commitment it fails at the commitment check, which precedes every other
  // check on the instance; under the right one it reaches nullifier creation
  // and fails there, which is what tells the two rejections apart.
  const resettlePrimaryFixture = async () => {
    const authority = Keypair.generate();
    await funder.fund(authority, 2);
    const { uploadId, txData } = await uploadTxData(authority, Buffer.from(fixture.tx_b64, "base64"));
    return settleFromTxDataBuilder(
      authority.publicKey,
      uploadId,
      txData,
      DUMMY_ROOT_MARKER,
      buildSettleRemainingAccounts(deriveNullifierAccounts(fixture.consumed_nullifiers_b64)),
    )
      .signers([authority])
      .rpc();
  };

  it("rejects set_kind_table_commitment from a non-authority signer", async () => {
    const stranger = Keypair.generate();
    await funder.fund(stranger, 1);
    await assertRejects(
      setKindTableCommitment(program, stranger.publicKey, randomRef()).signers([stranger]).rpc(),
      AUTHORITY_MISMATCH_PATTERN,
    );
    assert.deepEqual(
      (await program.account.paStateAccount.fetch(paState)).kindTableCommitment,
      empty,
      "the commitment is untouched",
    );
  });

  it("rejects a zero commitment", () =>
    assertRejects(
      setKindTableCommitment(program, provider.wallet.publicKey, Array(32).fill(0)).rpc(),
      /ZeroKindTableCommitment/,
    ));

  it("stores a new commitment, emits KindTableCommitmentUpdated, and rejects transactions proven against the previous table until it is restored", async () => {
    const rotated = randomRef();
    const sig = await setKindTableCommitment(program, provider.wallet.publicKey, rotated).rpc();
    assert.deepEqual(
      (await program.account.paStateAccount.fetch(paState)).kindTableCommitment,
      rotated,
      "the new commitment is stored",
    );
    const { events } = await cpiEventsOf(sig);
    const updated = events.find((e) => e.name === "kindTableCommitmentUpdatedEvent");
    assert.ok(
      updated,
      `a KindTableCommitmentUpdatedEvent is emitted; got ${events.map((e) => e.name).join(", ") || "none"}`,
    );
    // pa-evm: `event KindTableCommitmentUpdated(bytes32 kindTableCommitment)`.
    assert.deepEqual(
      updated!.data,
      { kindTableCommitment: rotated },
      "the event carries exactly pa-evm's field, the new commitment",
    );
    await assertRejects(resettlePrimaryFixture(), /KindTableCommitmentMismatch/);

    await setKindTableCommitment(program, provider.wallet.publicKey, empty).rpc();
    assert.deepEqual(
      (await program.account.paStateAccount.fetch(paState)).kindTableCommitment,
      empty,
      "the empty table's commitment is restored",
    );
    await assertRejects(resettlePrimaryFixture(), /DuplicateNullifier/);
  });
});
