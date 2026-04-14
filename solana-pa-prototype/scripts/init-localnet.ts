/**
 * Initialize on-chain state for localnet demo.
 *
 * This script is idempotent — safe to run multiple times.
 *
 * What it does:
 *   1. Funds the wallet via airdrop if needed
 *   2. Initializes PA state (if not already initialized)
 *   3. Initializes SPL Token Forwarder config (if not already initialized)
 *   4. Creates a deterministic test SPL token mint (6 decimals, USDC-like)
 *   5. Creates escrow ATA for the token on the forwarder's escrow PDA
 *   6. Creates fee payer ATA for the token
 *   7. Mints 1,000,000 test tokens to the fee payer ATA
 *
 * Usage:
 *   ANCHOR_PROVIDER_URL=http://localhost:8899 \
 *   ANCHOR_WALLET=~/.config/solana/id.json \
 *   npx ts-node -P tsconfig.json scripts/init-localnet.ts
 */
import * as anchor from "@coral-xyz/anchor";
import { Program } from "@coral-xyz/anchor";
import {
  PublicKey,
  SystemProgram,
  Keypair,
  LAMPORTS_PER_SOL,
} from "@solana/web3.js";
import {
  createMint,
  getOrCreateAssociatedTokenAccount,
  mintTo,
  createApproveInstruction,
} from "@solana/spl-token";
import { createHash } from "crypto";
import { existsSync, readFileSync } from "fs";
import path from "path";
import { SolanaPaPrototype } from "../target/types/solana_pa_prototype";
import { SplTokenForwarder } from "../target/types/spl_token_forwarder";
import {
  derivePaStatePda,
  deriveRootMarkerPda,
  deriveConfigPda,
  deriveEscrowPda,
} from "../tests/utils/pda";
import { EMPTY_TREE_ROOT_INITIAL } from "../tests/utils/constants";
import { VERIFIER_ROUTER_ID } from "./verifier-utils";

async function main() {
  const provider = anchor.AnchorProvider.env();
  anchor.setProvider(provider);

  const wallet = provider.wallet as anchor.Wallet;
  const connection = provider.connection;

  console.log("Wallet:    ", wallet.publicKey.toBase58());
  console.log("RPC:       ", connection.rpcEndpoint);

  const paProgram = anchor.workspace
    .SolanaPaPrototype as Program<SolanaPaPrototype>;
  const forwarderProgram = anchor.workspace
    .SplTokenForwarder as Program<SplTokenForwarder>;

  console.log("PA:        ", paProgram.programId.toBase58());
  console.log("Forwarder: ", forwarderProgram.programId.toBase58());

  // 1. Fund wallet if needed
  const balance = await connection.getBalance(wallet.publicKey);
  if (balance < 10 * LAMPORTS_PER_SOL) {
    console.log("Airdropping SOL to wallet...");
    const sig = await connection.requestAirdrop(
      wallet.publicKey,
      100 * LAMPORTS_PER_SOL
    );
    await connection.confirmTransaction(sig, "confirmed");
  }

  // 2. Initialize PA (idempotent)
  const verifierRouter = process.env.VERIFIER_ROUTER_PROGRAM
    ? new PublicKey(process.env.VERIFIER_ROUTER_PROGRAM)
    : VERIFIER_ROUTER_ID;
  const proofSelector = [0x73, 0xc4, 0x57, 0xba];

  const [paStatePda] = derivePaStatePda(paProgram.programId);
  try {
    await paProgram.account.paStateAccount.fetch(paStatePda);
    console.log("PA already initialized");
  } catch {
    console.log("Initializing PA...");
    console.log("  Verifier router:", verifierRouter.toBase58());
    const genesisRootMarkerPda = deriveRootMarkerPda(
      paStatePda,
      EMPTY_TREE_ROOT_INITIAL,
      paProgram.programId
    );
    await paProgram.methods
      .initialize(verifierRouter, proofSelector)
      .accounts({
        payer: wallet.publicKey,
      } as any)
      .remainingAccounts([
        {
          pubkey: genesisRootMarkerPda,
          isWritable: true,
          isSigner: false,
        },
      ])
      .rpc();
    console.log("PA initialized");
  }

  // 3. Load logic_ref from env var (hex) or fall back to fixture
  let logicRef: Buffer;
  const logicRefHex = process.env.LOGIC_REF;
  if (logicRefHex) {
    logicRef = Buffer.from(logicRefHex, "hex");
    if (logicRef.length !== 32) {
      throw new Error(`LOGIC_REF must be 32 bytes (64 hex chars), got ${logicRef.length}`);
    }
    console.log("Using LOGIC_REF from environment");
  } else {
    const wrapFixturePath = path.resolve(
      process.cwd(),
      "tests",
      "fixtures",
      "spl_token_wrap.json"
    );
    if (!existsSync(wrapFixturePath)) {
      throw new Error(
        "Wrap fixture not found at " +
          wrapFixturePath +
          " — run fixture-gen first. Or set LOGIC_REF env var (hex)."
      );
    }
    const fixture = JSON.parse(readFileSync(wrapFixturePath, "utf8"));
    logicRef = Buffer.from(fixture.spl_token_wrap.logic_ref_b64, "base64");
    console.log("Using logic_ref from fixture");
  }

  // 4. Initialize forwarder config (idempotent)
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
        wallet.publicKey // emergency committee = wallet for localnet
      )
      .accounts({
        authority: wallet.publicKey,
      })
      .rpc();
    console.log("Forwarder initialized");
  }

  // 5. Create deterministic test token mint (6 decimals, USDC-like)
  const mintSeed = createHash("sha256")
    .update("anomapay-test-token-mint")
    .digest();
  const mintKeypair = Keypair.fromSeed(mintSeed);

  let tokenMint: PublicKey;
  const mintAccountInfo = await connection.getAccountInfo(
    mintKeypair.publicKey
  );
  if (mintAccountInfo) {
    tokenMint = mintKeypair.publicKey;
    console.log("Test token mint already exists:", tokenMint.toBase58());
  } else {
    tokenMint = await createMint(
      connection,
      wallet.payer,
      wallet.publicKey, // mint authority
      null, // no freeze authority
      6, // 6 decimals like USDC
      mintKeypair
    );
    console.log("Created test token mint:", tokenMint.toBase58());
  }

  // 6. Create escrow ATA (on the forwarder's escrow PDA)
  const [escrowPda] = deriveEscrowPda(forwarderProgram.programId, tokenMint);
  const escrowAta = await getOrCreateAssociatedTokenAccount(
    connection,
    wallet.payer,
    tokenMint,
    escrowPda,
    true // allowOwnerOffCurve for PDA
  );
  console.log("Escrow ATA:", escrowAta.address.toBase58());

  // 7. Create fee payer ATA and mint tokens
  const feePayerAta = await getOrCreateAssociatedTokenAccount(
    connection,
    wallet.payer,
    tokenMint,
    wallet.publicKey
  );
  console.log("Fee payer ATA:", feePayerAta.address.toBase58());

  const MINT_AMOUNT = 1_000_000 * 1_000_000; // 1M tokens with 6 decimals
  if (Number(feePayerAta.amount) < MINT_AMOUNT) {
    await mintTo(
      connection,
      wallet.payer,
      tokenMint,
      feePayerAta.address,
      wallet.publicKey, // mint authority
      MINT_AMOUNT
    );
    console.log("Minted 1,000,000 test tokens");
  } else {
    console.log("Sufficient tokens already minted");
  }

  // 8. Approve escrow PDA as delegate on fee payer's ATA (for wrap operations)
  const approveAmount = MINT_AMOUNT; // approve full balance
  const approveTx = new anchor.web3.Transaction().add(
    createApproveInstruction(
      feePayerAta.address, // source ATA
      escrowPda,           // delegate (escrow PDA)
      wallet.publicKey,    // owner
      approveAmount
    )
  );
  await provider.sendAndConfirm(approveTx);
  console.log(`Approved escrow PDA as delegate (${approveAmount} tokens)`);

  console.log("");
  console.log("=== Localnet initialized ===");
  console.log("PA Program:        ", paProgram.programId.toBase58());
  console.log("Forwarder Program: ", forwarderProgram.programId.toBase58());
  console.log("Token Mint:        ", tokenMint.toBase58());
  console.log("Fee Payer:         ", wallet.publicKey.toBase58());
}

main().catch((err) => {
  console.error("Init failed:", err);
  process.exit(1);
});
