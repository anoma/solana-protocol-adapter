/**
 * TxData uploads under the default expiry bounds: expiry validation at init
 * and extend, closing, and the authority and bounds constraints.
 */
import * as anchor from "@anchor-lang/core";
import { SystemProgram, Keypair } from "@solana/web3.js";
import { assert } from "chai";
import { MIN_EXPIRY_SLOTS, MAX_EXPIRY_SLOTS } from "../client/constants";
import { deriveTxDataPda } from "../client/pda";
import { freshUploadId, assertFails } from "./utils/helpers";
import {
  provider,
  program,
  paState,
  DUMMY_ROOT_MARKER,
  settleFromTxDataBuilder,
  useAdapterSuite,
} from "./utils/adapterSuite";

describe("TxData lifecycle", () => {
  const { funder, initTxData } = useAdapterSuite();

  // The tests below assume the expiry bounds initialize sets. They are read,
  // not set: setting them is owner-only, and a cluster run's wallet owns
  // nothing.
  before(async () => {
    const state = await program.account.paStateAccount.fetch(paState);
    assert.equal(state.minExpirySlots.toNumber(), MIN_EXPIRY_SLOTS, "min_expiry_slots is the initial bound");
    assert.equal(state.maxExpirySlots.toNumber(), MAX_EXPIRY_SLOTS, "max_expiry_slots is the initial bound");
  });

  describe("protocol-adapter (TxData Expiration)", () => {
    it("rejects txdata_init with expires_slot too soon", async () => {
      const authority = await funder.fresh(1);

      const { uploadId, txData } = freshUploadId(program.programId, authority.publicKey);

      const slot = await provider.connection.getSlot("confirmed");
      // Set expiry too soon (only 50 slots from now, MIN is 100)
      const expiresSlot = new anchor.BN(slot + 50);

      await assertFails(
        program.methods
          .txdataInit(uploadId, 100, expiresSlot)
          .accountsPartial({
            paState,
            txData,
            authority: authority.publicKey,
            systemProgram: SystemProgram.programId,
          })
          .signers([authority])
          .rpc(),
        { program, error: "TxDataExpiryTooSoon" },
      );
    });

    it("rejects txdata_init with expires_slot too late", async () => {
      const authority = await funder.fresh(1);

      const { uploadId, txData } = freshUploadId(program.programId, authority.publicKey);

      const slot = await provider.connection.getSlot("confirmed");
      // Set expiry too late (MAX + 1000 slots from now)
      const expiresSlot = new anchor.BN(slot + MAX_EXPIRY_SLOTS + 1000);

      await assertFails(
        program.methods
          .txdataInit(uploadId, 100, expiresSlot)
          .accountsPartial({
            paState,
            txData,
            authority: authority.publicKey,
            systemProgram: SystemProgram.programId,
          })
          .signers([authority])
          .rpc(),
        { program, error: "TxDataExpiryTooLate" },
      );
    });

    it("accepts txdata_init with valid expires_slot", async () => {
      const authority = await funder.fresh(1);

      const slot = await provider.connection.getSlot("confirmed");
      const expiresSlot = new anchor.BN(slot + Math.floor((MIN_EXPIRY_SLOTS + MAX_EXPIRY_SLOTS) / 2));
      const { txData } = await initTxData(authority, 100, expiresSlot);

      const txDataAccount = await program.account.txDataAccount.fetch(txData);
      assert.equal(
        txDataAccount.expiresSlot.toNumber(),
        expiresSlot.toNumber(),
        "expires_slot should be set correctly",
      );
    });

    it("allows authority to close TxData anytime", async () => {
      const authority = await funder.fresh(2);

      const { uploadId, txData } = await initTxData(authority, 100);

      const before = await provider.connection.getAccountInfo(txData);
      assert.ok(before, "TxData should exist before close");

      const balanceBefore = await provider.connection.getBalance(authority.publicKey);

      await program.methods
        .txdataClose(uploadId)
        .accountsPartial({
          txData,
          authority: authority.publicKey,
          refund: authority.publicKey,
        })
        .signers([authority])
        .rpc();

      const after = await provider.connection.getAccountInfo(txData);
      assert.ok(!after, "TxData should not exist after close");

      const balanceAfter = await provider.connection.getBalance(authority.publicKey);
      assert.ok(balanceAfter > balanceBefore, "Authority balance should increase after close (rent refund)");
    });

    // Two-layer security model:
    // 1. PDA derivation includes authority's pubkey in seeds — attacker derives different PDA
    // 2. Anchor's seeds constraint verifies PDA matches signer — can't pass someone else's PDA

    it("rejects txdata_close for non-existent account (attacker's PDA doesn't exist)", async () => {
      // SCENARIO: Attacker derives their OWN PDA (using their pubkey in seeds).
      // Since they never created a TxData at that address, it doesn't exist.
      // This tests Anchor's account existence check.
      const [authority, attacker] = await Promise.all([funder.fresh(2), funder.fresh(1)]);

      const { uploadId, uploadIdLe } = await initTxData(authority, 100);

      // Attacker derives THEIR OWN PDA (different address because attacker.pubkey != authority.pubkey)
      const attackerTxData = deriveTxDataPda(program.programId, attacker.publicKey, uploadIdLe);

      // Attacker tries to close their own (non-existent) PDA
      await assertFails(
        program.methods
          .txdataClose(uploadId)
          .accountsPartial({
            txData: attackerTxData,
            authority: attacker.publicKey,
            refund: attacker.publicKey,
          })
          .signers([attacker])
          .rpc(),
        { program, error: "AccountNotInitialized" },
      );
    });

    it("rejects txdata_close when attacker passes authority's PDA directly (seed constraint)", async () => {
      // SCENARIO: Attacker KNOWS authority's TxData address and passes it directly.
      // But Anchor's seeds constraint computes [TX_DATA_SEED, signer.key(), upload_id].
      // Since signer is attacker, computed PDA != authority's PDA → ConstraintSeeds error.
      // This tests Anchor's PDA seed verification.
      const [authority, attacker] = await Promise.all([funder.fresh(2), funder.fresh(1)]);

      const { uploadId, txData: authorityTxData } = await initTxData(authority, 100);

      // Attacker tries to close AUTHORITY'S TxData by passing the address directly
      // Anchor will compute seeds with attacker.pubkey → different PDA → constraint fails
      await assertFails(
        program.methods
          .txdataClose(uploadId)
          .accountsPartial({
            txData: authorityTxData, // <-- Attacker passes authority's actual TxData
            authority: attacker.publicKey,
            refund: attacker.publicKey,
          })
          .signers([attacker])
          .rpc(),
        { program, error: "ConstraintSeeds" },
      );
    });

    it("extends TxData expiration deadline successfully", async () => {
      const authority = await funder.fresh(2);

      const slot = await provider.connection.getSlot("confirmed");
      const initialExpiry = new anchor.BN(slot + 1000);
      const { uploadId, txData } = await initTxData(authority, 100, initialExpiry);

      let txDataAccount = await program.account.txDataAccount.fetch(txData);
      assert.equal(txDataAccount.expiresSlot.toNumber(), initialExpiry.toNumber());

      const currentSlot = await provider.connection.getSlot("confirmed");
      const newExpiry = new anchor.BN(currentSlot + 5000);

      await program.methods
        .txdataExtend(uploadId, newExpiry)
        .accountsStrict({
          paState,
          txData,
          authority: authority.publicKey,
        })
        .signers([authority])
        .rpc();

      txDataAccount = await program.account.txDataAccount.fetch(txData);
      assert.equal(txDataAccount.expiresSlot.toNumber(), newExpiry.toNumber(), "expires_slot should be updated");
    });

    it("rejects txdata_extend that doesn't increase expires_slot", async () => {
      const authority = await funder.fresh(2);

      const slot = await provider.connection.getSlot("confirmed");
      const initialExpiry = new anchor.BN(slot + 10000);
      const { uploadId, txData } = await initTxData(authority, 100, initialExpiry);

      const currentSlot = await provider.connection.getSlot("confirmed");
      const lowerExpiry = new anchor.BN(currentSlot + 500); // Less than current expires_slot

      await assertFails(
        program.methods
          .txdataExtend(uploadId, lowerExpiry)
          .accountsStrict({
            paState,
            txData,
            authority: authority.publicKey,
          })
          .signers([authority])
          .rpc(),
        { program, error: "TxDataExtendMustIncrease" },
      );
    });

    it("rejects txdata_close_expired for non-expired TxData", async () => {
      const [authority, cleaner] = await Promise.all([funder.fresh(2), funder.fresh(1)]);

      const slot = await provider.connection.getSlot("confirmed");
      const { uploadId, txData } = await initTxData(authority, 100, new anchor.BN(slot + 50000));

      await assertFails(
        program.methods
          .txdataCloseExpired(uploadId, authority.publicKey)
          .accountsStrict({
            txData,
            payer: cleaner.publicKey,
            refund: authority.publicKey,
          })
          .signers([cleaner])
          .rpc(),
        { program, error: "TxDataNotExpired" },
      );
    });
  });

  describe("protocol-adapter (TxData authority and bounds checks)", () => {
    it("rejects txdata_write that exceeds payload capacity", async () => {
      const authority = await funder.fresh(2);

      const { uploadId, txData } = await initTxData(authority, 100);

      await assertFails(
        program.methods
          .txdataWrite(uploadId, 0, Buffer.alloc(200))
          .accountsPartial({
            txData,
            authority: authority.publicKey,
          })
          .signers([authority])
          .rpc(),
        { program, error: "TxDataBoundsExceeded" },
      );
    });

    it("rejects txdata_write from wrong authority", async () => {
      const [authority, wrongAuthority] = await Promise.all([funder.fresh(2), funder.fresh(1)]);

      const { uploadId, txData } = await initTxData(authority, 100);

      // Try to write as wrongAuthority — seed derivation uses signer's key,
      // which produces a different PDA, causing ConstraintSeeds
      await assertFails(
        program.methods
          .txdataWrite(uploadId, 0, Buffer.alloc(10))
          .accountsPartial({
            txData,
            authority: wrongAuthority.publicKey,
          })
          .signers([wrongAuthority])
          .rpc(),
        { program, error: "ConstraintSeeds" },
      );
    });

    it("rejects settle_from_txdata from wrong authority", async () => {
      const [authority, wrongAuthority] = await Promise.all([funder.fresh(2), funder.fresh(2)]);

      const { uploadId, txData } = await initTxData(authority, 100);

      await program.methods
        .txdataWrite(uploadId, 0, Buffer.alloc(50))
        .accountsPartial({
          txData,
          authority: authority.publicKey,
        })
        .signers([authority])
        .rpc();

      await assertFails(
        settleFromTxDataBuilder(wrongAuthority.publicKey, uploadId, txData, DUMMY_ROOT_MARKER, [], false)
          .signers([wrongAuthority])
          .rpc(),
        { program, error: "ConstraintSeeds" },
      );
    });

    it("rejects txdata_close with wrong refund address", async () => {
      const authority = await funder.fresh(2);
      const otherPubkey = Keypair.generate().publicKey;

      const { uploadId, txData } = await initTxData(authority, 100);

      await assertFails(
        program.methods
          .txdataClose(uploadId)
          .accountsPartial({
            txData,
            authority: authority.publicKey,
            refund: otherPubkey,
          })
          .signers([authority])
          .rpc(),
        { program, error: "ConstraintAddress" },
      );
    });

    it("rejects txdata_close_expired with wrong refund address", async () => {
      const [authority, cleaner] = await Promise.all([funder.fresh(2), funder.fresh(1)]);
      const wrongRefund = Keypair.generate().publicKey;

      const slot = await provider.connection.getSlot("confirmed");
      const { uploadId, txData } = await initTxData(authority, 100, new anchor.BN(slot + 50_000));

      // Hits ConstraintAddress before TxDataNotExpired
      // because Anchor validates account constraints before running the handler body
      await assertFails(
        program.methods
          .txdataCloseExpired(uploadId, authority.publicKey)
          .accountsStrict({
            txData,
            payer: cleaner.publicKey,
            refund: wrongRefund,
          })
          .signers([cleaner])
          .rpc(),
        { program, error: "ConstraintAddress" },
      );
    });
  });
});
