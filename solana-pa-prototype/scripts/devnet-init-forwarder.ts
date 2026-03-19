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
 * Reads program IDs from keypair files in target/deploy/ rather than Anchor.toml,
 * so it works regardless of the [provider] cluster setting.
 */
import * as anchor from "@coral-xyz/anchor";
import { Keypair, PublicKey } from "@solana/web3.js";
import { getOrCreateAssociatedTokenAccount } from "@solana/spl-token";
import { readFileSync } from "fs";
import path from "path";
import { deriveConfigPda, deriveEscrowPda } from "../tests/utils/pda";

// Read program ID from keypair file (same method as devnet.sh get_program_id)
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

  const paProgramId = loadProgramId("solana_pa_prototype");
  const forwarderProgramId = loadProgramId("spl_token_forwarder");

  // Load the forwarder IDL and construct a Program manually.
  // We can't use anchor.workspace because it resolves program IDs from
  // Anchor.toml's [provider] cluster, which may not match the deployed keypair.
  const forwarderIdlPath = path.resolve(
    __dirname,
    "..",
    "target",
    "idl",
    "spl_token_forwarder.json"
  );
  const forwarderIdl = JSON.parse(readFileSync(forwarderIdlPath, "utf8"));
  // Override the program address in the IDL to match the deployed keypair
  forwarderIdl.address = forwarderProgramId.toBase58();
  const forwarderProgram = new anchor.Program(forwarderIdl, provider);

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
  console.log("PA:        ", paProgramId.toBase58());
  console.log("Forwarder: ", forwarderProgramId.toBase58());
  console.log("Token mint:", tokenMint.toBase58());

  // 1. Initialize forwarder config (idempotent)
  const [configPda] = deriveConfigPda(forwarderProgramId);
  try {
    const configAccount = await connection.getAccountInfo(configPda);
    if (configAccount) {
      console.log("Forwarder already initialized");
    } else {
      throw new Error("not found");
    }
  } catch {
    console.log("Initializing forwarder...");
    await forwarderProgram.methods
      .initialize(
        paProgramId,
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
  const [escrowPda] = deriveEscrowPda(forwarderProgramId, tokenMint);
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
