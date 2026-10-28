/**
 * The kind tree root setter. The before hook settles the resubmitted
 * fixture unless it is settled already, so a re-submission of it reaches
 * nullifier creation exactly when the stored root matches its instance.
 */
import { assert } from "chai";
import { setKindTreeRoot } from "../client/instructions";
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

describe("protocol-adapter (kind tree root) @localnet", () => {
  const { funder, uploadTxData, settleFixture } = useAdapterSuite();
  let fixture: Fixture;

  // Mirrors pa-evm's setKindTreeRoot: owner-only, zero rejected,
  // KindTreeRootUpdated emitted. A transaction settles when it is
  // proven against the stored kind tree or against the empty one, as
  // pa-evm's _isKindTreeRootAccepted. The committed fixtures are proven
  // against the empty kind tree. The tests change the stored root and
  // restore the one the deployment held.
  let found: number[];
  const stored = async () => (await program.account.paStateAccount.fetch(paState)).kindTreeRoot;

  before(async () => {
    found = await stored();
    fixture = await loadFixture("batch_groth16_resubmitted.json");
    await settleFixture("batch_groth16_resubmitted.json");
  });

  after(async () => {
    if (!Buffer.from(await stored()).equals(Buffer.from(found))) {
      await setKindTreeRoot(program, provider.wallet.publicKey, found).rpc();
    }
  });

  // A fresh upload of `submitted`, by default the already-settled resubmitted
  // fixture. The root check precedes every other check on the
  // instance, so a transaction it accepts reaches nullifier creation and
  // fails there, and one it refuses fails with UnacceptedKindTreeRoot.
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

  it("rejects set_kind_tree_root from a non-authority signer", async () => {
    const stranger = await funder.fresh(1);
    await assertFails(setKindTreeRoot(program, stranger.publicKey, randomRef()).signers([stranger]).rpc(), {
      program,
      error: "OwnableUnauthorizedAccount",
    });
    assert.deepEqual(await stored(), found, "the root is untouched");
  });

  it("rejects a zero root", () =>
    assertFails(setKindTreeRoot(program, provider.wallet.publicKey, Array(32).fill(0)).rpc(), {
      program: program,
      error: "ZeroKindTreeRootNotAllowed",
    }));

  it("stores a new root, emits KindTreeRootUpdated, and still accepts a transaction proven against the empty kind tree", async () => {
    const rotated = randomRef();
    const sig = await setKindTreeRoot(program, provider.wallet.publicKey, rotated).rpc();
    assert.deepEqual(await stored(), rotated, "the new root is stored");
    const { events } = await cpiEventsOf(sig);
    const updated = events.find((e) => e.name === "kindTreeRootUpdated");
    assert.ok(updated, `a KindTreeRootUpdated event is emitted; got ${events.map((e) => e.name).join(", ") || "none"}`);
    // pa-evm: `event KindTreeRootUpdated(bytes32 indexed kindTreeRoot)`.
    assert.deepEqual(
      updated!.data,
      { kindTreeRoot: rotated },
      "the event carries exactly pa-evm's field, the new root",
    );
    // The fixture, proven against the empty kind tree, passes the root
    // check under the new root and reaches nullifier creation.
    await assertFails(resettleFixture(), { program, error: "PreExistingNullifier" });

    await setKindTreeRoot(program, provider.wallet.publicKey, found).rpc();
    assert.deepEqual(await stored(), found, "the root the deployment held is restored");
  });

  // pa-evm's UnacceptedKindTreeRoot: the transaction claims a kind
  // tree that is neither the stored one nor the empty one (fixture-gen's
  // foreign_kind_table error variant), refused before its proof is verified.
  it("refuses a transaction proven against a kind tree neither stored nor empty", async () =>
    assertFails(resettleFixture(await loadFixture("foreign_kind_table.json")), {
      program,
      error: "UnacceptedKindTreeRoot",
    }));
});
