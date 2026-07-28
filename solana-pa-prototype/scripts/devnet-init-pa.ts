import * as anchor from "@coral-xyz/anchor";
import { Program } from "@coral-xyz/anchor";
import { PublicKey } from "@solana/web3.js";
import { SolanaPaPrototype } from "../target/types/solana_pa_prototype";

const PA_STATE_SEED = Buffer.from("pa_state");

async function main() {
  const provider = anchor.AnchorProvider.env();
  anchor.setProvider(provider);

  const program = anchor.workspace.SolanaPaPrototype as Program<SolanaPaPrototype>;

  const [paState] = PublicKey.findProgramAddressSync(
    [PA_STATE_SEED],
    program.programId
  );

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

  await program.methods
    .initialize()
    .rpc();

  console.log("✅ PA initialized");
}

main().catch((err) => {
  console.error("❌ PA initialization failed:", err.message || err);
  process.exit(1);
});
