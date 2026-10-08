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

  // Mirrors pa-evm's setKindTableCommitment: owner-only, zero rejected,
  // KindTableCommitmentUpdated emitted. A transaction settles when it is
  // proven against the stored kind table or against the empty one, as
  // pa-evm's _isKindTableCommitmentAccepted. The committed fixtures are proven
  // against the empty table. The tests change the stored commitment and
  // restore the one the deployment held.
  let found: number[];
  const stored = async () => (await program.account.paStateAccount.fetch(paState)).kindTableCommitment;

  before(async () => {
    found = await stored();
    fixture = await loadFixture("batch_groth16_resubmitted.json");
    await settleFixture("batch_groth16_resubmitted.json");
  });

  after(async () => {
    if (!Buffer.from(await stored()).equals(Buffer.from(found))) {
      await setKindTableCommitment(program, provider.wallet.publicKey, found).rpc();
    }
  });

  // A fresh upload of `submitted`, by default the already-settled resubmitted
  // fixture. The commitment check precedes every other check on the
  // instance, so a transaction it accepts reaches nullifier creation and
  // fails there, and one it refuses fails with UnacceptedKindTableCommitment.
  const resettleFixture = async (submitted: Fixture = fixture) => {
    const authority = await funder.fresh(2);
    const { uploadId, txData } = await uploadTxData(authority, Buffer.from(submitted.tx_b64, "base64"));
    return settleFromTxDataBuilder(
      authority.publicKey,
      uploadId,
      txData,
      DUMMY_ROOT_MARKER,
      buildSettleRemainingAccounts(deriveNullifierAccounts(submitted.consumed_nullifiers_b64)),
    )
      .signers([authority])
      .rpc();
  };

  it("rejects set_kind_table_commitment from a non-authority signer", async () => {
    const stranger = await funder.fresh(1);
    await assertFails(setKindTableCommitment(program, stranger.publicKey, randomRef()).signers([stranger]).rpc(), {
      program,
      error: "OwnableUnauthorizedAccount",
    });
    assert.deepEqual(await stored(), found, "the commitment is untouched");
  });

  it("rejects a zero commitment", () =>
    assertFails(setKindTableCommitment(program, provider.wallet.publicKey, Array(32).fill(0)).rpc(), {
      program: program,
      error: "ZeroKindTableCommitmentNotAllowed",
    }));

  it("stores a new commitment, emits KindTableCommitmentUpdated, and still accepts a transaction proven against the empty table", async () => {
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
    // The fixture, proven against the empty table, passes the commitment
    // check under the new commitment and reaches nullifier creation.
    await assertFails(resettleFixture(), { program, error: "PreExistingNullifier" });

    await setKindTableCommitment(program, provider.wallet.publicKey, found).rpc();
    assert.deepEqual(await stored(), found, "the commitment the deployment held is restored");
  });

  // pa-evm's UnacceptedKindTableCommitment: the transaction claims a kind
  // table that is neither the stored one nor the empty one (fixture-gen's
  // foreign_kind_table error variant), refused before its proof is verified.
  it("refuses a transaction proven against a kind table neither stored nor empty", async () =>
    assertFails(resettleFixture(await loadFixture("foreign_kind_table.json")), {
      program,
      error: "UnacceptedKindTableCommitment",
    }));
});
