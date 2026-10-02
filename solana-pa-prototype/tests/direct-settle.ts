/**
 * `settle` with inline transaction data, and the double-spend rejection.
 * The before hook settles the resubmitted fixture unless it is settled
 * already, and the duplicate test re-submits its nullifiers.
 */
import { type Fixture, loadFixture } from "./utils/fixtures";
import { assertFails } from "./utils/helpers";
import {
  program,
  DUMMY_ROOT_MARKER,
  deriveNullifierAccounts,
  buildSettleRemainingAccounts,
  settleBuilder,
  settleFromTxDataBuilder,
  useAdapterSuite,
} from "./utils/adapterSuite";

describe("protocol-adapter (Direct settle & duplicate nullifier)", () => {
  const { funder, uploadTxData, settleFixture } = useAdapterSuite();
  let fixture: Fixture;

  before(async () => {
    fixture = await loadFixture("batch_groth16_resubmitted.json");
    await settleFixture("batch_groth16_resubmitted.json");
  });

  it("rejects garbage transaction_data via settle", async () => {
    const payer = await funder.fresh(2);

    await assertFails(
      settleBuilder(payer.publicKey, Buffer.from([0, 1, 2, 3]))
        .signers([payer])
        .rpc(),
      { program, error: "InvalidTransactionData" },
    );
  });

  it("rejects empty transaction (zero actions) via settle", async () => {
    const payer = await funder.fresh(2);

    // The fixture's aggregated transaction with its instance's action list
    // emptied (fixture-gen's zero_action.json error variant, SEC-006
    // regression): deserializes cleanly, rejected by the PA's empty-instance
    // check.
    const emptyTx = Buffer.from((await loadFixture("zero_action.json")).tx_b64, "base64");

    await assertFails(settleBuilder(payer.publicKey, emptyTx).signers([payer]).rpc(), {
      program,
      error: "InvalidTransactionData",
    });
  });

  it("rejects duplicate nullifier (double-spend) via settle_from_txdata", async () => {
    // The before hook consumed the fixture's nullifiers. Re-uploading the same tx
    // to a fresh TxData and trying to settle must fail at nullifier creation
    // because those nullifier PDAs already exist.
    const authority = await funder.fresh(2);

    const tx = Buffer.from(fixture.tx_b64, "base64");
    const { uploadId, txData } = await uploadTxData(authority, tx);

    // The SAME nullifier PDAs (created by the before hook's settlement)
    const nullifierAccounts = deriveNullifierAccounts(fixture.consumed_nullifiers_b64);
    const allRemainingAccounts = buildSettleRemainingAccounts(nullifierAccounts);

    await assertFails(
      settleFromTxDataBuilder(authority.publicKey, uploadId, txData, DUMMY_ROOT_MARKER, allRemainingAccounts)
        .signers([authority])
        .rpc(),
      { program, error: "DuplicateNullifier" },
    );
  });
});
