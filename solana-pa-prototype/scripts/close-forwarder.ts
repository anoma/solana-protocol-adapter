/**
 * Close all accounts owned by the SPL Token Forwarder program, reclaiming rent.
 *
 * Usage:
 *   ANCHOR_PROVIDER_URL=https://api.devnet.solana.com \
 *   ANCHOR_WALLET=scripts/devnet-wallet.json \
 *   TOKEN_MINT=4zMMC9srt5Ri5X14GAgXhaHii3GnPAEERYPJgZJDncDU \
 *   npx ts-node -P tsconfig.json scripts/close-forwarder.ts
 *
 * Required:
 *   TOKEN_MINT - SPL token mint address for the escrow to close
 *
 * What it does:
 *   1. Finds all 32-byte nonce bitmap accounts via getProgramAccounts
 *   2. Closes nonce bitmaps in batches (via close_nonce_bitmaps_batch)
 *   3. Closes escrow: drains tokens to caller's ATA, closes escrow ATA (via close_escrow)
 *   4. Closes Config PDA (via close_config) — last
 *
 * The wallet must be the emergency_committee stored in the forwarder Config.
 *
 * Requires `anchor build` to have been run after adding the close instructions,
 * so the IDL at target/idl/spl_token_forwarder.json includes them.
 */
import * as anchor from "@coral-xyz/anchor";
import { Keypair, PublicKey, LAMPORTS_PER_SOL } from "@solana/web3.js";
import {
  getAssociatedTokenAddress,
  getOrCreateAssociatedTokenAccount,
  TOKEN_PROGRAM_ID,
} from "@solana/spl-token";
import { readFileSync } from "fs";
import path from "path";
import { deriveConfigPda, deriveEscrowPda } from "../tests/utils/pda";

const NONCE_BITMAP_SIZE = 32;

function loadProgramId(name: string): PublicKey {
  const keypairPath = path.resolve(
    __dirname,
    "..",
    "target",
    "deploy",
    `${name}-keypair.json`
  );
  const keypairData = JSON.parse(readFileSync(keypairPath, "utf8"));
  return Keypair.fromSecretKey(Uint8Array.from(keypairData)).publicKey;
}

async function main() {
  const provider = anchor.AnchorProvider.env();
  anchor.setProvider(provider);

  const wallet = provider.wallet as anchor.Wallet;
  const connection = provider.connection;

  const forwarderProgramId = loadProgramId("spl_token_forwarder");

  // Load IDL manually (same pattern as devnet-init-forwarder.ts)
  const forwarderIdlPath = path.resolve(
    __dirname,
    "..",
    "target",
    "idl",
    "spl_token_forwarder.json"
  );
  const forwarderIdl = JSON.parse(readFileSync(forwarderIdlPath, "utf8"));
  forwarderIdl.address = forwarderProgramId.toBase58();
  const program = new anchor.Program(forwarderIdl, provider);

  // Require TOKEN_MINT
  const tokenMintStr = process.env.TOKEN_MINT;
  if (!tokenMintStr) {
    console.error("❌ TOKEN_MINT env var is required (base58 mint address)");
    console.error(
      "  Devnet USDC:  4zMMC9srt5Ri5X14GAgXhaHii3GnPAEERYPJgZJDncDU"
    );
    console.error(
      "  Mainnet USDC: EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v"
    );
    process.exit(1);
  }
  const tokenMint = new PublicKey(tokenMintStr);

  console.log("Forwarder:", forwarderProgramId.toBase58());
  console.log("Wallet:   ", wallet.publicKey.toBase58());
  console.log("Token mint:", tokenMint.toBase58());

  // Verify config exists and wallet is emergency_committee
  const [configPda] = deriveConfigPda(forwarderProgramId);
  const configInfo = await connection.getAccountInfo(configPda);
  if (!configInfo) {
    console.log("Config not found — nothing to close.");
    return;
  }

  // Read emergency_committee from raw config data.
  // Layout: discriminator(8) + protocol_adapter(32) + logic_ref(32) + emergency_committee(32)
  const emergencyCommitteeBytes = configInfo.data.subarray(72, 104);
  const emergencyCommittee = new PublicKey(emergencyCommitteeBytes);
  if (!emergencyCommittee.equals(wallet.publicKey)) {
    console.error(
      `❌ Wallet ${wallet.publicKey.toBase58()} is not the emergency committee ` +
        `(${emergencyCommittee.toBase58()})`
    );
    process.exit(1);
  }

  let totalRecovered = 0;

  // 1. Close nonce bitmap PDAs (32-byte accounts owned by forwarder)
  const bitmaps = await connection.getProgramAccounts(forwarderProgramId, {
    filters: [{ dataSize: NONCE_BITMAP_SIZE }],
  });
  console.log(`\nFound ${bitmaps.length} nonce bitmap accounts`);

  if (bitmaps.length > 0) {
    const BATCH_SIZE = 20;
    const bitmapRent =
      await connection.getMinimumBalanceForRentExemption(NONCE_BITMAP_SIZE);
    console.log(`Closing in batches of ${BATCH_SIZE}...`);

    const bitmapPubkeys = bitmaps.map(({ pubkey }) => pubkey);
    for (let i = 0; i < bitmapPubkeys.length; i += BATCH_SIZE) {
      const batch = bitmapPubkeys.slice(i, i + BATCH_SIZE);
      const remainingAccounts = batch.map((pubkey) => ({
        pubkey,
        isWritable: true,
        isSigner: false,
      }));

      try {
        await program.methods
          .closeNonceBitmapsBatch()
          .accounts({
            authority: wallet.publicKey,
            config: configPda,
          })
          .remainingAccounts(remainingAccounts)
          .rpc();

        const batchLamports = batch.length * bitmapRent;
        totalRecovered += batchLamports;
        console.log(
          `  Batch ${Math.floor(i / BATCH_SIZE) + 1}: ` +
            `${batch.length} bitmaps (~${(batchLamports / LAMPORTS_PER_SOL).toFixed(6)} SOL)`
        );
      } catch (e: any) {
        console.error(
          `  ❌ Batch ${Math.floor(i / BATCH_SIZE) + 1} failed: ${e.message}`
        );
      }
    }
  }

  // 2. Close escrow (drain tokens + close ATA)
  const [escrowPda] = deriveEscrowPda(forwarderProgramId, tokenMint);
  const escrowAta = await getAssociatedTokenAddress(tokenMint, escrowPda, true);

  const escrowAtaInfo = await connection.getAccountInfo(escrowAta);
  if (escrowAtaInfo) {
    // Ensure caller has an ATA to receive drained tokens
    const recipientAtaAccount = await getOrCreateAssociatedTokenAccount(
      connection,
      wallet.payer,
      tokenMint,
      wallet.publicKey
    );
    const recipientAta = recipientAtaAccount.address;

    console.log("\nClosing escrow...");
    console.log(`  Escrow ATA:    ${escrowAta.toBase58()}`);
    console.log(`  Recipient ATA: ${recipientAta.toBase58()}`);

    const escrowAtaLamports = escrowAtaInfo.lamports;
    try {
      await program.methods
        .closeEscrow()
        .accounts({
          authority: wallet.publicKey,
          config: configPda,
          escrowAta: escrowAta,
          escrowPda: escrowPda,
          recipientAta: recipientAta,
          tokenMint: tokenMint,
          tokenProgram: TOKEN_PROGRAM_ID,
        })
        .rpc();

      totalRecovered += escrowAtaLamports;
      console.log(
        `  ✅ Escrow closed (~${(escrowAtaLamports / LAMPORTS_PER_SOL).toFixed(6)} SOL rent recovered)`
      );
    } catch (e: any) {
      console.error(`  ❌ Failed to close escrow: ${e.message}`);
    }
  } else {
    console.log("\nEscrow ATA not found — skipping.");
  }

  // 3. Close config last
  console.log("\nClosing config...");
  const configLamports = configInfo.lamports;
  try {
    await program.methods
      .closeConfig()
      .accounts({
        authority: wallet.publicKey,
        config: configPda,
      })
      .rpc();

    totalRecovered += configLamports;
    console.log(
      `  ✅ Config closed (${(configLamports / LAMPORTS_PER_SOL).toFixed(6)} SOL)`
    );
  } catch (e: any) {
    console.error(`  ❌ Failed to close config: ${e.message}`);
  }

  console.log(
    `\n✅ Total recovered: ${(totalRecovered / LAMPORTS_PER_SOL).toFixed(6)} SOL`
  );
}

main().catch((err) => {
  console.error("Close forwarder failed:", err);
  process.exit(1);
});
