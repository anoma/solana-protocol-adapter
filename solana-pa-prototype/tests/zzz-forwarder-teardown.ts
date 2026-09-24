/**
 * SPL token forwarder teardown: the committee reclaims rent from nonce
 * bitmaps, escrows and finally the config. Runs last — nothing survives it.
 */
import * as anchor from "@coral-xyz/anchor";
import { Program } from "@coral-xyz/anchor";
import { Keypair, PublicKey } from "@solana/web3.js";
import { createMint, getAccount, getOrCreateAssociatedTokenAccount } from "@solana/spl-token";
import { assert } from "chai";
import { SplTokenForwarder } from "../target/types/spl_token_forwarder";
import {
  EMERGENCY_COMMITTEE_LABEL,
  assertRejects,
  closeAllNonceBitmaps,
  closeEscrow,
  createFundedEscrow,
  deriveConfigPda,
  escrowAccounts,
  makeFunder,
  seededKeypair,
  closeConfig,
} from "./utils";

describe("zzz-forwarder-teardown (reclaims forwarder rent)", () => {
  const provider = anchor.AnchorProvider.env();
  anchor.setProvider(provider);

  const forwarderProgram = anchor.workspace.SplTokenForwarder as Program<SplTokenForwarder>;
  const [configPda] = deriveConfigPda(forwarderProgram.programId);
  const funder = makeFunder(provider);

  const emergencyCommittee = seededKeypair(EMERGENCY_COMMITTEE_LABEL);
  const impostor = Keypair.generate();

  let escrow: { mint: PublicKey; escrowPda: PublicKey; escrowAta: PublicKey; recipientAta: PublicKey };
  const ESCROW_FUNDING = 500_000_000n;

  before(async () => {
    await funder.fund(emergencyCommittee, 5);
    await funder.fund(impostor, 1);
    assert.ok(
      await provider.connection.getAccountInfo(configPda),
      "01-spl-token-forwarder.ts must have initialized the forwarder config"
    );

    const funded = await createFundedEscrow(provider, forwarderProgram.programId, emergencyCommittee, ESCROW_FUNDING);
    const recipientAta = (
      await getOrCreateAssociatedTokenAccount(provider.connection, emergencyCommittee, funded.mint, emergencyCommittee.publicKey)
    ).address;
    escrow = { ...funded, recipientAta };
  });

  const closeEscrowAs = (authority: Keypair, accounts = escrow) =>
    closeEscrow(forwarderProgram, configPda, authority.publicKey, accounts).signers([authority]).rpc();

  const closeConfigAs = (authority: Keypair) =>
    closeConfig(forwarderProgram, authority.publicKey).signers([authority]).rpc();

  it("close_escrow rejects a non-committee authority", () => assertRejects(closeEscrowAs(impostor), /UnauthorizedCaller/));

  it("close_nonce_bitmaps_batch rejects a non-committee authority", () =>
    assertRejects(closeAllNonceBitmaps(forwarderProgram, configPda, impostor.publicKey, [impostor]), /UnauthorizedCaller/));

  it("close_config rejects a non-committee authority", () => assertRejects(closeConfigAs(impostor), /UnauthorizedCaller/));

  it("close_nonce_bitmaps_batch rejects a program account that is not a bitmap", () =>
    // The config PDA is program-owned but is not a nonce bitmap.
    assertRejects(
      forwarderProgram.methods
        .closeNonceBitmapsBatch()
        .accountsPartial({ authority: emergencyCommittee.publicKey, config: configPda })
        .remainingAccounts([{ pubkey: configPda, isWritable: true, isSigner: false }])
        .signers([emergencyCommittee])
        .rpc(),
      /InvalidNonceBitmapPda/
    ));

  it("closes the nonce bitmaps the wrap tests created and refunds their rent", async () => {
    const committeeBefore = await provider.connection.getBalance(emergencyCommittee.publicKey);

    const closed = await closeAllNonceBitmaps(forwarderProgram, configPda, emergencyCommittee.publicKey, [emergencyCommittee]);

    assert.isAbove(closed, 0, "the adapter suite's wrap must have created at least one nonce bitmap");
    assert.isEmpty(await forwarderProgram.account.nonceBitmap.all(), "every nonce bitmap is closed");
    assert.isAbove(
      await provider.connection.getBalance(emergencyCommittee.publicKey),
      committeeBefore,
      "the committee recovers more rent than it pays in fees"
    );
  });

  it("close_escrow drains the tokens to the recipient and closes the account", async () => {
    assert.equal((await getAccount(provider.connection, escrow.escrowAta)).amount, ESCROW_FUNDING);
    const recipientBefore = (await getAccount(provider.connection, escrow.recipientAta)).amount;
    const committeeBefore = await provider.connection.getBalance(emergencyCommittee.publicKey);

    await closeEscrowAs(emergencyCommittee);

    assert.isNull(await provider.connection.getAccountInfo(escrow.escrowAta), "escrow ATA is closed");
    const recipientAfter = (await getAccount(provider.connection, escrow.recipientAta)).amount;
    assert.equal(recipientAfter - recipientBefore, ESCROW_FUNDING, "every token is drained to the recipient");
    assert.isAbove(await provider.connection.getBalance(emergencyCommittee.publicKey), committeeBefore, "rent returns to the committee");
  });

  it("close_escrow closes an empty escrow", async () => {
    const mint = await createMint(provider.connection, emergencyCommittee, emergencyCommittee.publicKey, null, 6);
    const empty = escrowAccounts(forwarderProgram.programId, mint);
    await getOrCreateAssociatedTokenAccount(provider.connection, emergencyCommittee, mint, empty.escrowPda, true);
    const recipientAta = (
      await getOrCreateAssociatedTokenAccount(provider.connection, emergencyCommittee, mint, emergencyCommittee.publicKey)
    ).address;
    assert.equal((await getAccount(provider.connection, empty.escrowAta)).amount, 0n);

    await closeEscrowAs(emergencyCommittee, { mint, ...empty, recipientAta });

    assert.isNull(await provider.connection.getAccountInfo(empty.escrowAta), "empty escrow ATA is closed");
  });

  it("close_config closes the config last and refunds its rent", async () => {
    const committeeBefore = await provider.connection.getBalance(emergencyCommittee.publicKey);

    await closeConfigAs(emergencyCommittee);

    assert.isNull(await provider.connection.getAccountInfo(configPda), "config PDA is closed");
    assert.isAbove(await provider.connection.getBalance(emergencyCommittee.publicKey), committeeBefore, "rent returns to the committee");
  });

  after(() => funder.drainAll());
});
