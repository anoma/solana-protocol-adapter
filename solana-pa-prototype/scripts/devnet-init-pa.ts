import * as anchor from "@coral-xyz/anchor";
import { Program } from "@coral-xyz/anchor";
import { PublicKey } from "@solana/web3.js";
import { SolanaPaPrototype } from "../target/types/solana_pa_prototype";

const EMPTY_TREE_ROOT_INITIAL = Buffer.from(
  "cc1d2f838445db7aec431df9ee8a871f40e7aa5e064fc056633ef8c60fab7b06",
  "hex"
);

const PA_STATE_SEED = Buffer.from("pa_state");
const ROOT_SEED = Buffer.from("root");

async function main() {
  const provider = anchor.AnchorProvider.env();
  anchor.setProvider(provider);

  const program = anchor.workspace.SolanaPaPrototype as Program<SolanaPaPrototype>;

  const [paState] = PublicKey.findProgramAddressSync(
    [PA_STATE_SEED],
    program.programId
  );

  const genesisRootMarkerPda = PublicKey.findProgramAddressSync(
    [ROOT_SEED, paState.toBuffer(), EMPTY_TREE_ROOT_INITIAL],
    program.programId
  )[0];

  // Idempotent: skip if PAState already exists
  try {
    await program.account.paStateAccount.fetch(paState);
    console.log("PAState already initialized, skipping.");
    return;
  } catch {
    // Not initialized yet — proceed
  }

  console.log("Initializing PA...");
  console.log(`  PAState PDA: ${paState.toBase58()}`);
  console.log(`  Genesis root marker: ${genesisRootMarkerPda.toBase58()}`);

  await program.methods
    .initialize()
    .remainingAccounts([
      { pubkey: genesisRootMarkerPda, isWritable: true, isSigner: false },
    ])
    .rpc();

  console.log("✅ PA initialized");
}

main().catch((err) => {
  console.error("❌ PA initialization failed:", err.message || err);
  process.exit(1);
});
