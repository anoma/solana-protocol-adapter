/**
 * SPL Token Forwarder Teardown Tests
 *
 * Runs LAST — destroys forwarder state (config, escrows, nonce bitmaps).
 *
 * Verifies:
 *   - close_escrow drains tokens before closing the ATA
 *   - close_escrow works on empty escrow (no drain needed)
 *   - close_nonce_bitmaps_batch closes bitmaps from prior wrap tests
 *   - close_config closes the config PDA
 *   - All three reject non-emergency-committee callers
 *   - Rent lamports are recovered to the authority
 *
 * Dependencies:
 *   - Config initialized by 01-spl-token-forwarder.ts (same emergency_committee seed)
 *   - Nonce bitmaps may exist from wrap operations in prior tests
 */
import * as anchor from "@coral-xyz/anchor";
import { Program } from "@coral-xyz/anchor";
import { PublicKey, Keypair, LAMPORTS_PER_SOL } from "@solana/web3.js";
import {
  createMint,
  getOrCreateAssociatedTokenAccount,
  mintTo,
  getAccount,
  TOKEN_PROGRAM_ID,
} from "@solana/spl-token";
import { assert } from "chai";
import { createHash } from "crypto";
import { SplTokenForwarder } from "../target/types/spl_token_forwarder";
import { deriveConfigPda, deriveEscrowPda } from "./utils";
import { fundKeypair, drainKeypairs } from "./utils";

const NONCE_BITMAP_SIZE = 32;

// Must match 01-spl-token-forwarder.ts and zz-forwarder-emergency.ts
const EMERGENCY_COMMITTEE_SEED = createHash("sha256")
  .update("emergency_committee_seed")
  .digest();

describe("zzz-forwarder-teardown (runs last - destroys forwarder state)", () => {
  const provider = anchor.AnchorProvider.env();
  anchor.setProvider(provider);

  const localFundedKeypairs: Keypair[] = [];
  async function localAirdrop(kp: Keypair, sol: number) {
    await fundKeypair(provider, kp, sol);
    localFundedKeypairs.push(kp);
  }

  let program: Program<SplTokenForwarder>;
  let emergencyCommittee: Keypair;
  let configPda: PublicKey;

  // Escrow close test state
  let tokenMint: PublicKey;
  let escrowPda: PublicKey;
  let escrowAta: PublicKey;
  let recipientAta: PublicKey;
  const ESCROW_MINT_AMOUNT = 500_000_000n; // 500 tokens (6 decimals)

  before(async () => {
    program = anchor.workspace.SplTokenForwarder as Program<SplTokenForwarder>;

    emergencyCommittee = Keypair.fromSeed(EMERGENCY_COMMITTEE_SEED);
    await localAirdrop(emergencyCommittee, 5);

    [configPda] = deriveConfigPda(program.programId);

    // Config must exist from earlier tests
    const configInfo = await provider.connection.getAccountInfo(configPda);
    if (!configInfo) {
      throw new Error(
        "Forwarder config not found — earlier tests should have initialized it"
      );
    }

    // Create a fresh token mint for the escrow close test
    tokenMint = await createMint(
      provider.connection,
      emergencyCommittee,
      emergencyCommittee.publicKey,
      null,
      6
    );

    [escrowPda] = deriveEscrowPda(program.programId, tokenMint);

    const escrowAtaAccount = await getOrCreateAssociatedTokenAccount(
      provider.connection,
      emergencyCommittee,
      tokenMint,
      escrowPda,
      true // allowOwnerOffCurve for PDA
    );
    escrowAta = escrowAtaAccount.address;

    // Mint tokens into escrow
    await mintTo(
      provider.connection,
      emergencyCommittee,
      tokenMint,
      escrowAta,
      emergencyCommittee,
      Number(ESCROW_MINT_AMOUNT)
    );

    // Create recipient ATA for the emergency committee
    const recipientAtaAccount = await getOrCreateAssociatedTokenAccount(
      provider.connection,
      emergencyCommittee,
      tokenMint,
      emergencyCommittee.publicKey
    );
    recipientAta = recipientAtaAccount.address;
  });

  // =========================================================================
  // Auth rejection (config is still alive for all three)
  // =========================================================================

  it("close_escrow rejects non-committee authority", async () => {
    const impostor = Keypair.generate();
    await localAirdrop(impostor, 1);

    try {
      await program.methods
        .closeEscrow()
        .accounts({
          authority: impostor.publicKey,
          config: configPda,
          escrowAta: escrowAta,
          escrowPda: escrowPda,
          recipientAta: recipientAta,
          tokenMint: tokenMint,
          tokenProgram: TOKEN_PROGRAM_ID,
        })
        .signers([impostor])
        .rpc();
      assert.fail("Expected UnauthorizedCaller");
    } catch (e: any) {
      assert.include(e.toString(), "UnauthorizedCaller");
    }
  });

  it("close_nonce_bitmaps_batch rejects non-committee authority", async () => {
    const impostor = Keypair.generate();
    await localAirdrop(impostor, 1);

    try {
      await program.methods
        .closeNonceBitmapsBatch()
        .accounts({
          authority: impostor.publicKey,
          config: configPda,
        })
        .signers([impostor])
        .rpc();
      assert.fail("Expected UnauthorizedCaller");
    } catch (e: any) {
      assert.include(e.toString(), "UnauthorizedCaller");
    }
  });

  it("close_config rejects non-committee authority", async () => {
    const impostor = Keypair.generate();
    await localAirdrop(impostor, 1);

    try {
      await program.methods
        .closeConfig()
        .accounts({
          authority: impostor.publicKey,
          config: configPda,
        })
        .signers([impostor])
        .rpc();
      assert.fail("Expected UnauthorizedCaller");
    } catch (e: any) {
      assert.include(e.toString(), "UnauthorizedCaller");
    }
  });

  // =========================================================================
  // close_nonce_bitmaps_batch — close bitmaps from prior wrap tests
  // =========================================================================

  it("close_nonce_bitmaps_batch rejects non-bitmap accounts (wrong data size)", async () => {
    // The config PDA is program-owned but not a 32-byte bitmap.
    // Passing it as a remaining_account should fail the size check.
    try {
      await program.methods
        .closeNonceBitmapsBatch()
        .accounts({
          authority: emergencyCommittee.publicKey,
          config: configPda,
        })
        .remainingAccounts([
          {
            pubkey: configPda,
            isWritable: true,
            isSigner: false,
          },
        ])
        .signers([emergencyCommittee])
        .rpc();
      assert.fail("Expected InvalidNonceBitmapPda");
    } catch (e: any) {
      assert.include(e.toString(), "InvalidNonceBitmapPda");
    }
  });

  it("closes nonce bitmap PDAs from previous tests", async () => {
    const bitmaps = await provider.connection.getProgramAccounts(
      program.programId,
      { filters: [{ dataSize: NONCE_BITMAP_SIZE }] }
    );

    console.log(`  Found ${bitmaps.length} nonce bitmap accounts to close`);

    if (bitmaps.length === 0) {
      console.log("  No nonce bitmaps found — skipping batch close");
      return;
    }

    const BATCH_SIZE = 20;
    const bitmapRent =
      await provider.connection.getMinimumBalanceForRentExemption(
        NONCE_BITMAP_SIZE
      );
    const committeeBefore = await provider.connection.getBalance(
      emergencyCommittee.publicKey
    );

    for (let i = 0; i < bitmaps.length; i += BATCH_SIZE) {
      const batch = bitmaps.slice(i, i + BATCH_SIZE);
      const remainingAccounts = batch.map(({ pubkey }) => ({
        pubkey,
        isWritable: true,
        isSigner: false,
      }));

      await program.methods
        .closeNonceBitmapsBatch()
        .accounts({
          authority: emergencyCommittee.publicKey,
          config: configPda,
        })
        .remainingAccounts(remainingAccounts)
        .signers([emergencyCommittee])
        .rpc();
    }

    // All bitmaps gone
    const remaining = await provider.connection.getProgramAccounts(
      program.programId,
      { filters: [{ dataSize: NONCE_BITMAP_SIZE }] }
    );
    assert.equal(remaining.length, 0, "All nonce bitmaps should be closed");

    // Rent recovered (net of tx fees)
    const committeeAfter = await provider.connection.getBalance(
      emergencyCommittee.publicKey
    );
    const expectedRecovery = bitmaps.length * bitmapRent;
    const actualRecovery = committeeAfter - committeeBefore;
    console.log(
      `  Recovered ${(actualRecovery / LAMPORTS_PER_SOL).toFixed(6)} SOL ` +
        `(expected ~${(expectedRecovery / LAMPORTS_PER_SOL).toFixed(
          6
        )} minus tx fees)`
    );
    assert.ok(actualRecovery > 0, "Should recover more rent than tx fees");
  });

  // =========================================================================
  // close_escrow — drain tokens + close ATA
  // =========================================================================

  it("close_escrow rejects wrong token mint (PDA seeds mismatch)", async () => {
    // Create a different mint that doesn't match the escrow PDA derivation
    const wrongMint = await createMint(
      provider.connection,
      emergencyCommittee,
      emergencyCommittee.publicKey,
      null,
      6
    );

    // escrowPda was derived from tokenMint, not wrongMint.
    // Passing wrongMint causes the seeds constraint on escrow_pda to fail
    // because find_program_address(["escrow", wrongMint]) != escrowPda.
    try {
      await program.methods
        .closeEscrow()
        .accounts({
          authority: emergencyCommittee.publicKey,
          config: configPda,
          escrowAta: escrowAta,
          escrowPda: escrowPda,
          recipientAta: recipientAta,
          tokenMint: wrongMint,
          tokenProgram: TOKEN_PROGRAM_ID,
        })
        .signers([emergencyCommittee])
        .rpc();
      assert.fail("Expected seeds constraint failure");
    } catch (e: any) {
      assert.include(e.toString(), "ConstraintSeeds");
    }
  });

  it("drains tokens and closes escrow ATA", async () => {
    // Verify escrow has tokens
    const escrowBefore = await getAccount(provider.connection, escrowAta);
    assert.equal(
      escrowBefore.amount.toString(),
      ESCROW_MINT_AMOUNT.toString(),
      "Escrow should have tokens before close"
    );

    const recipientBefore = await getAccount(provider.connection, recipientAta);
    const recipientBalanceBefore = BigInt(recipientBefore.amount.toString());
    const committeeSolBefore = await provider.connection.getBalance(
      emergencyCommittee.publicKey
    );

    await program.methods
      .closeEscrow()
      .accounts({
        authority: emergencyCommittee.publicKey,
        config: configPda,
        escrowAta: escrowAta,
        escrowPda: escrowPda,
        recipientAta: recipientAta,
        tokenMint: tokenMint,
        tokenProgram: TOKEN_PROGRAM_ID,
      })
      .signers([emergencyCommittee])
      .rpc();

    // Escrow ATA no longer exists
    const escrowAfterInfo = await provider.connection.getAccountInfo(escrowAta);
    assert.isNull(escrowAfterInfo, "Escrow ATA should be closed");

    // Tokens drained to recipient
    const recipientAfter = await getAccount(provider.connection, recipientAta);
    const recipientBalanceAfter = BigInt(recipientAfter.amount.toString());
    assert.equal(
      (recipientBalanceAfter - recipientBalanceBefore).toString(),
      ESCROW_MINT_AMOUNT.toString(),
      "All tokens should be drained to recipient"
    );

    // Rent recovered to authority
    const committeeSolAfter = await provider.connection.getBalance(
      emergencyCommittee.publicKey
    );
    const solRecovered = committeeSolAfter - committeeSolBefore;
    assert.ok(
      solRecovered > 0,
      "Emergency committee should receive rent from closed ATA"
    );
    console.log(
      `  Drained ${ESCROW_MINT_AMOUNT} tokens, ` +
        `recovered ${(solRecovered / LAMPORTS_PER_SOL).toFixed(6)} SOL rent`
    );
  });

  it("closes empty escrow ATA (no drain needed)", async () => {
    // Fresh mint with empty escrow
    const emptyMint = await createMint(
      provider.connection,
      emergencyCommittee,
      emergencyCommittee.publicKey,
      null,
      6
    );

    const [emptyEscrowPda] = deriveEscrowPda(program.programId, emptyMint);
    const emptyEscrowAtaAccount = await getOrCreateAssociatedTokenAccount(
      provider.connection,
      emergencyCommittee,
      emptyMint,
      emptyEscrowPda,
      true
    );
    const emptyEscrowAta = emptyEscrowAtaAccount.address;

    const emptyRecipientAtaAccount = await getOrCreateAssociatedTokenAccount(
      provider.connection,
      emergencyCommittee,
      emptyMint,
      emergencyCommittee.publicKey
    );

    // Verify escrow is empty
    const escrowAccount = await getAccount(provider.connection, emptyEscrowAta);
    assert.equal(escrowAccount.amount.toString(), "0");

    await program.methods
      .closeEscrow()
      .accounts({
        authority: emergencyCommittee.publicKey,
        config: configPda,
        escrowAta: emptyEscrowAta,
        escrowPda: emptyEscrowPda,
        recipientAta: emptyRecipientAtaAccount.address,
        tokenMint: emptyMint,
        tokenProgram: TOKEN_PROGRAM_ID,
      })
      .signers([emergencyCommittee])
      .rpc();

    // Escrow ATA gone
    const afterInfo = await provider.connection.getAccountInfo(emptyEscrowAta);
    assert.isNull(afterInfo, "Empty escrow ATA should be closed");
  });

  // =========================================================================
  // close_config — must be last (other close instructions need it)
  // =========================================================================

  it("closes config PDA (last)", async () => {
    const configInfo = await provider.connection.getAccountInfo(configPda);
    assert.isNotNull(configInfo, "Config should exist before close");
    const configLamports = configInfo!.lamports;

    const committeeSolBefore = await provider.connection.getBalance(
      emergencyCommittee.publicKey
    );

    await program.methods
      .closeConfig()
      .accounts({
        authority: emergencyCommittee.publicKey,
        config: configPda,
      })
      .signers([emergencyCommittee])
      .rpc();

    // Config gone
    const configAfter = await provider.connection.getAccountInfo(configPda);
    assert.isNull(configAfter, "Config PDA should be closed");

    // Rent recovered
    const committeeSolAfter = await provider.connection.getBalance(
      emergencyCommittee.publicKey
    );
    const recovered = committeeSolAfter - committeeSolBefore;
    console.log(
      `  Config closed, recovered ${(recovered / LAMPORTS_PER_SOL).toFixed(
        6
      )} SOL ` +
        `(config was ${(configLamports / LAMPORTS_PER_SOL).toFixed(6)} SOL)`
    );
    assert.ok(recovered > 0, "Should recover rent from config close");
  });

  after(async () => {
    await drainKeypairs(provider, localFundedKeypairs, "zzz-teardown");
    localFundedKeypairs.length = 0;
  });
});
