import * as anchor from "@coral-xyz/anchor";
import { Program } from "@coral-xyz/anchor";
import { PublicKey } from "@solana/web3.js";
import { SolanaPaPrototype } from "../target/types/solana_pa_prototype";

const PA_STATE_SEED = Buffer.from("pa_state");
const BPF_LOADER_UPGRADEABLE = new PublicKey(
  "BPFLoaderUpgradeab1e11111111111111111111111"
);

async function main() {
  const provider = anchor.AnchorProvider.env();
  anchor.setProvider(provider);

  const program = anchor.workspace.SolanaPaPrototype as Program<SolanaPaPrototype>;

  const [paState] = PublicKey.findProgramAddressSync(
    [PA_STATE_SEED],
    program.programId
  );
  const [programData] = PublicKey.findProgramAddressSync(
    [program.programId.toBuffer()],
    BPF_LOADER_UPGRADEABLE
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
    .accounts({
      paState,
      payer: provider.wallet.publicKey,
      systemProgram: anchor.web3.SystemProgram.programId,
      program: program.programId,
      programData,
    })
    .rpc();

  console.log("✅ PA initialized");
}

main().catch((err) => {
  console.error("❌ PA initialization failed:", err.message || err);
  process.exit(1);
});
