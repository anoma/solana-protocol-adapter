/**
 * SPL token forwarder teardown: the committee reclaims rent from nonce
 * bitmaps, escrows and finally the config. Runs last — nothing survives it.
 */
import * as anchor from "@coral-xyz/anchor";
import { Program } from "@coral-xyz/anchor";
import { Keypair, PublicKey } from "@solana/web3.js";
import { createMint, getAccount, getOrCreateAssociatedTokenAccount, TOKEN_PROGRAM_ID } from "@solana/spl-token";
import { assert } from "chai";
import { SplTokenForwarder } from "../target/types/spl_token_forwarder";
import {
  EMERGENCY_COMMITTEE_LABEL,
  NONCE_BITMAP_ACCOUNT_SIZE,
  closeAllNonceBitmaps,
  createFundedEscrow,
  deriveConfigPda,
  escrowAccounts,
  makeFunder,
  seededKeypair,
} from "./utils";

describe("zzz-forwarder-teardown (reclaims forwarder rent)", () => {
  const provider = anchor.AnchorProvider.env();
  anchor.setProvider(provider);

  const forwarderProgram = anchor.workspace.SplTokenForwarder as Program<SplTokenForwarder>;
  const [configPda] = deriveConfigPda(forwarderProgram.programId);
  const funder = makeFunder(provider);

  const emergencyCommittee = seededKeypair(EMERGENCY_COMMITTEE_LABEL);
  const impostor = Keypair.generate();

  let tokenMint: PublicKey;
  let escrowPda: PublicKey;
  let escrowAta: PublicKey;
  let recipientAta: PublicKey;
  const ESCROW_FUNDING = 500_000_000n;

  before(async () => {
    await funder.fund(emergencyCommittee, 5);
    await funder.fund(impostor, 1);
    assert.ok(
      await provider.connection.getAccountInfo(configPda),
      "01-spl-token-forwarder.ts must have initialized the forwarder config"
    );

    ({ mint: tokenMint, escrowPda, escrowAta } = await createFundedEscrow(
      provider,
      forwarderProgram.programId,
      emergencyCommittee,
      ESCROW_FUNDING
    ));
    recipientAta = (
      await getOrCreateAssociatedTokenAccount(provider.connection, emergencyCommittee, tokenMint, emergencyCommittee.publicKey)
    ).address;
  });

  function closeEscrow(authority: Keypair, accounts: { escrowAta: PublicKey; escrowPda: PublicKey; recipientAta: PublicKey; tokenMint: PublicKey }) {
    return forwarderProgram.methods
      .closeEscrow()
      .accountsPartial({
        authority: authority.publicKey,
        config: configPda,
        escrowAta: accounts.escrowAta,
        escrowPda: accounts.escrowPda,
        recipientAta: accounts.recipientAta,
        tokenMint: accounts.tokenMint,
        tokenProgram: TOKEN_PROGRAM_ID,
      })
      .signers([authority])
      .rpc();
  }

  it("close_escrow rejects a non-committee authority", async () => {
    try {
      await closeEscrow(impostor, { escrowAta, escrowPda, recipientAta, tokenMint });
      assert.fail("expected close_escrow to fail");
    } catch (e: any) {
      assert.include(e.toString(), "UnauthorizedCaller");
    }
  });

  it("close_nonce_bitmaps_batch rejects a non-committee authority", async () => {
    try {
      await closeAllNonceBitmaps(forwarderProgram, configPda, impostor.publicKey, [impostor]);
      assert.fail("expected close_nonce_bitmaps_batch to fail");
    } catch (e: any) {
      assert.include(e.toString(), "UnauthorizedCaller");
    }
  });

  it("close_config rejects a non-committee authority", async () => {
    try {
      await forwarderProgram.methods
        .closeConfig()
        .accountsPartial({ authority: impostor.publicKey, config: configPda })
        .signers([impostor])
        .rpc();
      assert.fail("expected close_config to fail");
    } catch (e: any) {
      assert.include(e.toString(), "UnauthorizedCaller");
    }
  });

  it("close_nonce_bitmaps_batch rejects a program account that is not a bitmap", async () => {
    // The config PDA is program-owned but is not a nonce bitmap.
    try {
      await forwarderProgram.methods
        .closeNonceBitmapsBatch()
        .accountsPartial({ authority: emergencyCommittee.publicKey, config: configPda })
        .remainingAccounts([{ pubkey: configPda, isWritable: true, isSigner: false }])
        .signers([emergencyCommittee])
        .rpc();
      assert.fail("expected close_nonce_bitmaps_batch to fail");
    } catch (e: any) {
      assert.include(e.toString(), "InvalidNonceBitmapPda");
    }
  });

  it("closes the nonce bitmaps the wrap tests created and refunds their rent", async () => {
    const committeeBefore = await provider.connection.getBalance(emergencyCommittee.publicKey);

    const closed = await closeAllNonceBitmaps(forwarderProgram, configPda, emergencyCommittee.publicKey, [emergencyCommittee]);

    assert.isAbove(closed, 0, "the adapter suite's wrap must have created at least one nonce bitmap");
    const remaining = await provider.connection.getProgramAccounts(forwarderProgram.programId, {
      filters: [{ dataSize: NONCE_BITMAP_ACCOUNT_SIZE }],
    });
    assert.equal(remaining.length, 0, "every nonce bitmap is closed");
    assert.isAbove(
      await provider.connection.getBalance(emergencyCommittee.publicKey),
      committeeBefore,
      "the committee recovers more rent than it pays in fees"
    );
  });

  it("close_escrow rejects a mint that does not match the escrow PDA", async () => {
    const wrongMint = await createMint(provider.connection, emergencyCommittee, emergencyCommittee.publicKey, null, 6);
    try {
      await closeEscrow(emergencyCommittee, { escrowAta, escrowPda, recipientAta, tokenMint: wrongMint });
      assert.fail("expected close_escrow to fail");
    } catch (e: any) {
      assert.include(e.toString(), "ConstraintSeeds");
    }
  });

  it("close_escrow drains the tokens to the recipient and closes the account", async () => {
    assert.equal((await getAccount(provider.connection, escrowAta)).amount, ESCROW_FUNDING);
    const recipientBefore = (await getAccount(provider.connection, recipientAta)).amount;
    const committeeBefore = await provider.connection.getBalance(emergencyCommittee.publicKey);

    await closeEscrow(emergencyCommittee, { escrowAta, escrowPda, recipientAta, tokenMint });

    assert.isNull(await provider.connection.getAccountInfo(escrowAta), "escrow ATA is closed");
    const recipientAfter = (await getAccount(provider.connection, recipientAta)).amount;
    assert.equal(recipientAfter - recipientBefore, ESCROW_FUNDING, "every token is drained to the recipient");
    assert.isAbove(await provider.connection.getBalance(emergencyCommittee.publicKey), committeeBefore, "rent returns to the committee");
  });

  it("close_escrow closes an empty escrow", async () => {
    const emptyMint = await createMint(provider.connection, emergencyCommittee, emergencyCommittee.publicKey, null, 6);
    const empty = escrowAccounts(forwarderProgram.programId, emptyMint);
    await getOrCreateAssociatedTokenAccount(provider.connection, emergencyCommittee, emptyMint, empty.escrowPda, true);
    const emptyRecipientAta = (
      await getOrCreateAssociatedTokenAccount(provider.connection, emergencyCommittee, emptyMint, emergencyCommittee.publicKey)
    ).address;
    assert.equal((await getAccount(provider.connection, empty.escrowAta)).amount, 0n);

    await closeEscrow(emergencyCommittee, { ...empty, recipientAta: emptyRecipientAta, tokenMint: emptyMint });

    assert.isNull(await provider.connection.getAccountInfo(empty.escrowAta), "empty escrow ATA is closed");
  });

  it("close_config closes the config last and refunds its rent", async () => {
    const committeeBefore = await provider.connection.getBalance(emergencyCommittee.publicKey);

    await forwarderProgram.methods
      .closeConfig()
      .accountsPartial({ authority: emergencyCommittee.publicKey, config: configPda })
      .signers([emergencyCommittee])
      .rpc();

    assert.isNull(await provider.connection.getAccountInfo(configPda), "config PDA is closed");
    assert.isAbove(await provider.connection.getBalance(emergencyCommittee.publicKey), committeeBefore, "rent returns to the committee");
  });

  after(() => funder.drainAll());
});
