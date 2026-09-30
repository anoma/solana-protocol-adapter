/**
 * Expired TxData: writes and settlement are rejected, anyone may close it,
 * and an extension must satisfy the bounds from the current slot. The
 * before hook lowers min_expiry_slots so uploads can expire within a test.
 */
import * as anchor from "@anchor-lang/core";
import { SystemProgram, Keypair, ComputeBudgetProgram } from "@solana/web3.js";
import { assert } from "chai";
import { MAX_EXPIRY_SLOTS, MIN_ALLOWED_EXPIRY } from "../client/constants";
import { VERIFIER_ROUTER_ID } from "../client/verifier";
import { waitForSlotPast, assertFails } from "./utils/helpers";
import {
  provider,
  program,
  paState,
  fixture,
  VERIFIER_PROGRAM_ID,
  routerPda,
  verifierEntryPda,
  DUMMY_ROOT_MARKER,
  deriveNullifierAccounts,
  ensureAdapterInitialized,
  setExpiryBounds,
  buildSettleRemainingAccounts,
  useAdapterSuite,
} from "./utils/adapterSuite";

describe("protocol-adapter (TxData expiration enforcement)", () => {
  const { funder, uploadTxData, initTxData } = useAdapterSuite();

  before(async () => {
    await ensureAdapterInitialized();
    // Lower min_expiry_slots so we can create short-lived TxData
    await setExpiryBounds(MIN_ALLOWED_EXPIRY, MAX_EXPIRY_SLOTS);
  });

  // On devnet, slots advance at ~2.5/s and tx confirmation takes seconds.
  // 30 slots gives enough room to init+write before expiration.
  const EXPIRY_OFFSET = 30;

  it("rejects txdata_write on expired TxData", async () => {
    const authority = Keypair.generate();
    await funder.fund(authority, 2);

    const slot = await provider.connection.getSlot("confirmed");
    const expiresSlot = new anchor.BN(slot + EXPIRY_OFFSET);
    const { uploadId, txData } = await initTxData(authority, 100, expiresSlot);

    await program.methods
      .txdataWrite(uploadId, 0, Buffer.alloc(10))
      .accountsPartial({
        txData,
        authority: authority.publicKey,
      })
      .signers([authority])
      .rpc();

    await waitForSlotPast(provider.connection, expiresSlot.toNumber());

    await assertFails(
      program.methods
        .txdataWrite(uploadId, 10, Buffer.alloc(10))
        .accountsPartial({
          txData,
          authority: authority.publicKey,
        })
        .signers([authority])
        .rpc(),
      { program, error: "TxDataExpired" },
    );
  });

  it("rejects settle_from_txdata on expired TxData", async () => {
    const authority = Keypair.generate();
    await funder.fund(authority, 2);

    const tx = Buffer.from(fixture.tx_b64, "base64");
    const slot = await provider.connection.getSlot("confirmed");
    const expiresSlot = new anchor.BN(slot + EXPIRY_OFFSET);

    const { uploadId, txData } = await uploadTxData(authority, tx, expiresSlot);

    await waitForSlotPast(provider.connection, expiresSlot.toNumber());

    const nullifierAccounts = deriveNullifierAccounts(fixture.consumed_nullifiers_b64);
    const allRemainingAccounts = buildSettleRemainingAccounts(nullifierAccounts);

    await assertFails(
      program.methods
        .settleFromTxdata(uploadId)
        .accountsPartial({
          paState,
          txData,
          authority: authority.publicKey,
          systemProgram: SystemProgram.programId,
          newRootMarker: DUMMY_ROOT_MARKER,
          verifierRouterProgram: VERIFIER_ROUTER_ID,
          router: routerPda,
          verifierEntry: verifierEntryPda,
          verifierProgram: VERIFIER_PROGRAM_ID,
        })
        .remainingAccounts(allRemainingAccounts)
        .preInstructions([
          ComputeBudgetProgram.setComputeUnitLimit({ units: 1_400_000 }),
          ComputeBudgetProgram.requestHeapFrame({ bytes: 256 * 1024 }),
        ])
        .signers([authority])
        .rpc(),
      { program, error: "TxDataExpired" },
    );
  });

  it("allows permissionless close of expired TxData", async () => {
    const authority = Keypair.generate();
    const cleaner = Keypair.generate();
    await Promise.all([funder.fund(authority, 2), funder.fund(cleaner, 1)]);

    const slot = await provider.connection.getSlot("confirmed");
    const expiresSlot = new anchor.BN(slot + EXPIRY_OFFSET);
    const { uploadId, txData } = await initTxData(authority, 100, expiresSlot);

    const before = await provider.connection.getAccountInfo(txData);
    assert.ok(before, "TxData should exist before close");

    const refundBalanceBefore = await provider.connection.getBalance(authority.publicKey);

    await waitForSlotPast(provider.connection, expiresSlot.toNumber());

    // Third-party (cleaner) calls txdata_close_expired — permissionless
    await program.methods
      .txdataCloseExpired(uploadId, authority.publicKey)
      .accountsStrict({
        txData,
        payer: cleaner.publicKey,
        refund: authority.publicKey,
      })
      .signers([cleaner])
      .rpc();

    const after = await provider.connection.getAccountInfo(txData);
    assert.ok(!after, "TxData should not exist after close_expired");

    const refundBalanceAfter = await provider.connection.getBalance(authority.publicKey);
    assert.ok(
      refundBalanceAfter > refundBalanceBefore,
      "Authority balance should increase after expired close (rent refund)",
    );
  });

  it("rejects txdata_extend with expires_slot too soon", async () => {
    const authority = Keypair.generate();
    await funder.fund(authority, 2);

    const { uploadId, txData, expiresSlot } = await initTxData(
      authority,
      100,
      new anchor.BN((await provider.connection.getSlot("confirmed")) + EXPIRY_OFFSET),
    );

    // Wait for the original expiry to pass so the extend would need to
    // satisfy bounds from current slot. Then try extending to current_slot + 5
    // which is below min_expiry_slots=10.
    await waitForSlotPast(provider.connection, expiresSlot.toNumber());

    const currentSlot = await provider.connection.getSlot("confirmed");
    const tooSoonExpiry = new anchor.BN(currentSlot + 5);

    await assertFails(
      program.methods
        .txdataExtend(uploadId, tooSoonExpiry)
        .accountsStrict({
          paState,
          txData,
          authority: authority.publicKey,
        })
        .signers([authority])
        .rpc(),
      { program, error: "TxDataExpiryTooSoon" },
    );
  });

  it("rejects txdata_extend with expires_slot too late", async () => {
    const authority = Keypair.generate();
    await funder.fund(authority, 2);

    const { uploadId, txData } = await initTxData(
      authority,
      100,
      new anchor.BN((await provider.connection.getSlot("confirmed")) + 1000),
    );

    const currentSlot = await provider.connection.getSlot("confirmed");
    const tooLateExpiry = new anchor.BN(currentSlot + MAX_EXPIRY_SLOTS + 100_000);

    await assertFails(
      program.methods
        .txdataExtend(uploadId, tooLateExpiry)
        .accountsStrict({
          paState,
          txData,
          authority: authority.publicKey,
        })
        .signers([authority])
        .rpc(),
      { program, error: "TxDataExpiryTooLate" },
    );
  });
});
