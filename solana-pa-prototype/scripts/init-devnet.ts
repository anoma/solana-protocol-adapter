/**
 * Initialize PA state on devnet
 *
 * This script initializes the Protocol Adapter state account on Solana devnet.
 * Run with: npx ts-node scripts/init-devnet.ts
 *
 * Prerequisites:
 * - Solana CLI configured for devnet: solana config set --url devnet
 * - Wallet with SOL: ~/.config/solana/id.json
 * - Programs deployed to devnet
 */

import * as anchor from "@coral-xyz/anchor";
import { Program } from "@coral-xyz/anchor";
import { PublicKey, SystemProgram, Connection, Keypair } from "@solana/web3.js";
import { readFileSync } from "fs";
import path from "path";
import { SolanaPaPrototype } from "../target/types/solana_pa_prototype";

// PADDING_LEAF = ZEROS[0] from merkle.rs - the genesis root for an empty tree
const PADDING_LEAF = Buffer.from(
  "cc1d2f838445db7aec431df9ee8a871f40e7aa5e064fc056633ef8c60fab7b06",
  "hex"
);

// Seeds for PDAs
const PA_STATE_SEED = Buffer.from("pa_state");
const ROOT_MARKER_SEED = Buffer.from("root");

async function main() {
  console.log("=== PA Devnet Initialization ===\n");

  // Load wallet from default Solana CLI location
  const walletPath = process.env.WALLET_PATH ||
    path.join(process.env.HOME || "", ".config", "solana", "id.json");

  const walletKeypair = Keypair.fromSecretKey(
    Uint8Array.from(JSON.parse(readFileSync(walletPath, "utf8")))
  );
  console.log(`Wallet: ${walletKeypair.publicKey.toBase58()}`);

  // Connect to devnet
  const connection = new Connection("https://api.devnet.solana.com", "confirmed");
  const balance = await connection.getBalance(walletKeypair.publicKey);
  console.log(`Balance: ${balance / 1e9} SOL\n`);

  // Set up Anchor provider
  const wallet = new anchor.Wallet(walletKeypair);
  const provider = new anchor.AnchorProvider(connection, wallet, {
    commitment: "confirmed",
  });
  anchor.setProvider(provider);

  // Load the program - use the devnet program ID
  const programId = new PublicKey("4Lc29cQ8ErbX23jcUXypYFB5kTGSrS8wD6EBHu6hR6Ns");

  // Load IDL
  const idlPath = path.resolve(process.cwd(), "target", "idl", "solana_pa_prototype.json");
  const idl = JSON.parse(readFileSync(idlPath, "utf8"));

  const program = new Program(idl, provider) as Program<SolanaPaPrototype>;
  console.log(`Program ID: ${program.programId.toBase58()}`);

  // Derive PAState PDA
  const [paState] = PublicKey.findProgramAddressSync([PA_STATE_SEED], program.programId);
  console.log(`PAState PDA: ${paState.toBase58()}`);

  // Compute genesis root (empty tree root)
  const genesisRoot = PADDING_LEAF;
  console.log(`Genesis Root: ${genesisRoot.toString("hex")}`);

  // Derive genesis root marker PDA
  const [genesisRootMarkerPda] = PublicKey.findProgramAddressSync(
    [ROOT_MARKER_SEED, paState.toBuffer(), genesisRoot],
    program.programId
  );
  console.log(`Genesis Root Marker PDA: ${genesisRootMarkerPda.toBase58()}\n`);

  // Check if already initialized
  try {
    const existingState = await program.account.paStateAccount.fetch(paState);
    console.log("PAState already initialized!");
    console.log(`  next_index: ${existingState.nextIndex}`);
    console.log(`  current_depth: ${existingState.currentDepth}`);
    console.log(`  current_root: ${Buffer.from(existingState.root).toString("hex")}`);
    return;
  } catch (err) {
    console.log("PAState not found - initializing...\n");
  }

  // Initialize
  try {
    const tx = await program.methods
      .initialize()
      .accountsStrict({
        paState,
        payer: wallet.publicKey,
        systemProgram: SystemProgram.programId,
      })
      .remainingAccounts([
        { pubkey: genesisRootMarkerPda, isWritable: true, isSigner: false },
      ])
      .rpc();

    console.log(`Initialized PA state!`);
    console.log(`Transaction: ${tx}`);
    console.log(`Explorer: https://explorer.solana.com/tx/${tx}?cluster=devnet\n`);

    // Verify
    const state = await program.account.paStateAccount.fetch(paState);
    console.log("PAState verified:");
    console.log(`  next_index: ${state.nextIndex}`);
    console.log(`  current_depth: ${state.currentDepth}`);
    console.log(`  current_root: ${Buffer.from(state.root).toString("hex")}`);
  } catch (err) {
    console.error("Failed to initialize:", err);
    process.exit(1);
  }
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
