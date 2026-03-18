/**
 * Initialize SPL Token Forwarder on-chain state for devnet.
 *
 * This script is idempotent — safe to run multiple times.
 *
 * What it does:
 *   1. Initializes forwarder config (PA program ID, logic_ref, emergency committee)
 *   2. Creates escrow ATA for the specified token mint
 *
 * Environment variables:
 *   ANCHOR_PROVIDER_URL  - Solana RPC URL (required)
 *   ANCHOR_WALLET        - Path to wallet keypair (required)
 *   LOGIC_REF            - 32-byte hex logic ref for authorized transfer logic (required)
 *   TOKEN_MINT           - SPL token mint address for escrow ATA (required)
 *
 * Usage:
 *   ANCHOR_PROVIDER_URL=https://api.devnet.solana.com \
 *   ANCHOR_WALLET=scripts/devnet-wallet.json \
 *   LOGIC_REF=8bceee49ac4646f7bf1ba20be658be5ab5699ce5cab58004f44efaa900717384 \
 *   TOKEN_MINT=4zMMC9srt5Ri5X14GAgXhaHii3GnPAEERYPJgZJDncDU \
 *   npx ts-node -P tsconfig.json scripts/devnet-init-forwarder.ts
 */
import * as anchor from "@coral-xyz/anchor";
import { Program } from "@coral-xyz/anchor";
import { PublicKey } from "@solana/web3.js";
import { getOrCreateAssociatedTokenAccount } from "@solana/spl-token";
import { SolanaPaPrototype } from "../target/types/solana_pa_prototype";
import { SplTokenForwarder } from "../target/types/spl_token_forwarder";
import { deriveConfigPda, deriveEscrowPda } from "../tests/utils/pda";

async function main() {
  const provider = anchor.AnchorProvider.env();
  anchor.setProvider(provider);

  const wallet = provider.wallet as anchor.Wallet;
  const connection = provider.connection;

  const paProgram = anchor.workspace
    .SolanaPaPrototype as Program<SolanaPaPrototype>;
  const forwarderProgram = anchor.workspace
    .SplTokenForwarder as Program<SplTokenForwarder>;

  // Require LOGIC_REF
  const logicRefHex = process.env.LOGIC_REF;
  if (!logicRefHex) {
    throw new Error("LOGIC_REF env var is required (64 hex chars)");
  }
  const logicRef = Buffer.from(logicRefHex, "hex");
  if (logicRef.length !== 32) {
    throw new Error(
      `LOGIC_REF must be 32 bytes (64 hex chars), got ${logicRef.length}`
    );
  }

  // Require TOKEN_MINT
  const tokenMintStr = process.env.TOKEN_MINT;
  if (!tokenMintStr) {
    throw new Error("TOKEN_MINT env var is required (base58 mint address)");
  }
  const tokenMint = new PublicKey(tokenMintStr);

  console.log("Wallet:    ", wallet.publicKey.toBase58());
  console.log("RPC:       ", connection.rpcEndpoint);
  console.log("PA:        ", paProgram.programId.toBase58());
  console.log("Forwarder: ", forwarderProgram.programId.toBase58());
  console.log("Token mint:", tokenMint.toBase58());

  // 1. Initialize forwarder config (idempotent)
  const [configPda] = deriveConfigPda(forwarderProgram.programId);
  try {
    await forwarderProgram.account.config.fetch(configPda);
    console.log("Forwarder already initialized");
  } catch {
    console.log("Initializing forwarder...");
    await forwarderProgram.methods
      .initialize(
        paProgram.programId,
        Array.from(logicRef),
        wallet.publicKey // emergency committee = deployer wallet
      )
      .accounts({
        authority: wallet.publicKey,
      })
      .rpc();
    console.log("✅ Forwarder initialized");
  }

  // 2. Create escrow ATA for the token mint (idempotent)
  const [escrowPda] = deriveEscrowPda(forwarderProgram.programId, tokenMint);
  const escrowAta = await getOrCreateAssociatedTokenAccount(
    connection,
    wallet.payer,
    tokenMint,
    escrowPda,
    true // allowOwnerOffCurve for PDA
  );
  console.log("Escrow ATA:", escrowAta.address.toBase58());

  console.log("");
  console.log("✅ Forwarder initialized");
  console.log("  Config PDA: ", configPda.toBase58());
  console.log("  Escrow PDA: ", escrowPda.toBase58());
  console.log("  Escrow ATA: ", escrowAta.address.toBase58());
  console.log("  Token mint: ", tokenMint.toBase58());
}

main().catch((err) => {
  console.error("❌ Forwarder initialization failed:", err.message || err);
  process.exit(1);
});
