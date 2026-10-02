/**
 * The kind-table commitment setter. The before hook settles the resubmitted
 * fixture unless it is settled already, so a re-submission of it reaches
 * nullifier creation exactly when the stored commitment matches its instance.
 */
import { assert } from "chai";
import { setKindTableCommitment } from "../client/instructions";
import { randomRef, assertFails } from "./utils/helpers";
import {
  provider,
  program,
  paState,
  DUMMY_ROOT_MARKER,
  deriveNullifierAccounts,
  buildSettleRemainingAccounts,
  cpiEventsOf,
  settleFromTxDataBuilder,
  useAdapterSuite,
} from "./utils/adapterSuite";
import { type Fixture, loadFixture } from "./utils/fixtures";

describe("protocol-adapter (kind table commitment) @localnet", () => {
  const { funder, uploadTxData, settleFixture } = useAdapterSuite();
  let fixture: Fixture;

  // Mirrors pa-evm ProtocolAdapter.setKindTableCommitment: owner-only, zero
  // rejected, KindTableCommitmentUpdated emitted. The stored commitment is
  // what every settled aggregation instance must carry, so a change rejects
  // transactions proven against the previous table until it is changed back.
  // The tests change it and restore the one the deployment held.
  let found: number[];
  const stored = async () => (await program.account.paStateAccount.fetch(paState)).kindTableCommitment;

  before(async () => {
    found = await stored();
    fixture = loadFixture("batch_groth16_resubmitted.json");
    await settleFixture("batch_groth16_resubmitted.json");
  });

  after(async () => {
    if (!Buffer.from(await stored()).equals(Buffer.from(found))) {
      await setKindTableCommitment(program, provider.wallet.publicKey, found).rpc();
    }
  });

  // A fresh upload of the already-settled resubmitted fixture. Under another
  // commitment it fails at the commitment check, which precedes every other
  // check on the instance; under the right one it reaches nullifier creation
  // and fails there, which is what tells the two rejections apart.
  const resettleFixture = async () => {
    const authority = await funder.fresh(2);
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
    const stranger = await funder.fresh(1);
    await assertFails(setKindTableCommitment(program, stranger.publicKey, randomRef()).signers([stranger]).rpc(), {
      program,
      error: "Unauthorized",
    });
    assert.deepEqual(await stored(), found, "the commitment is untouched");
  });

  it("rejects a zero commitment", () =>
    assertFails(setKindTableCommitment(program, provider.wallet.publicKey, Array(32).fill(0)).rpc(), {
      program: program,
      error: "ZeroKindTableCommitment",
    }));

  it("stores a new commitment, emits KindTableCommitmentUpdated, and rejects transactions proven against the previous table until it is restored", async () => {
    const rotated = randomRef();
    const sig = await setKindTableCommitment(program, provider.wallet.publicKey, rotated).rpc();
    assert.deepEqual(await stored(), rotated, "the new commitment is stored");
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
    await assertFails(resettleFixture(), { program: program, error: "KindTableCommitmentMismatch" });

    await setKindTableCommitment(program, provider.wallet.publicKey, found).rpc();
    assert.deepEqual(await stored(), found, "the commitment the deployment held is restored");
    await assertFails(resettleFixture(), { program: program, error: "DuplicateNullifier" });
  });
});
