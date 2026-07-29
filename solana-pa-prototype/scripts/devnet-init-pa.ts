import * as anchor from "@coral-xyz/anchor";
import { Program } from "@coral-xyz/anchor";
import { PublicKey } from "@solana/web3.js";
import { SolanaPaPrototype } from "../target/types/solana_pa_prototype";

const PA_STATE_SEED = Buffer.from("pa_state");
const BPF_LOADER_UPGRADEABLE = new PublicKey(
  "BPFLoaderUpgradeab1e11111111111111111111111"
);

// `initialize` pins the router and selector this deployment will trust for
// the lifetime of the PAState account. There is no safe default: guessing
// wrong installs the wrong verifier. Both must be supplied explicitly.
function requireVerifierRouter(): PublicKey {
  const raw = process.env.PA_VERIFIER_ROUTER;
  if (!raw) {
    console.error(
      "❌ Missing PA_VERIFIER_ROUTER: the RISC0 verifier router program ID " +
        "this deployment must trust, as a base58 pubkey.\n" +
        "   This script will not guess a default — initializing against the " +
        "wrong router installs the wrong verifier."
    );
    process.exit(1);
  }
  try {
    return new PublicKey(raw);
  } catch {
    console.error(`❌ PA_VERIFIER_ROUTER is not a valid pubkey: "${raw}"`);
    process.exit(1);
  }
}

function requireProofSelector(): number[] {
  const raw = process.env.PA_PROOF_SELECTOR;
  if (!raw) {
    console.error(
      "❌ Missing PA_PROOF_SELECTOR: the 4-byte Groth16 verifier selector " +
        "(hex, e.g. 0xdeadbeef) registered with the verifier router for the " +
        "circuit this deployment must accept.\n" +
        "   This script will not guess a default — initializing with the " +
        "wrong selector installs the wrong verifier."
    );
    process.exit(1);
  }
  const hex = raw.replace(/^0x/, "");
  if (hex.length !== 8) {
    console.error(
      `❌ PA_PROOF_SELECTOR must be 8 hex chars (4 bytes), got "${raw}"`
    );
    process.exit(1);
  }
  return Array.from(Buffer.from(hex, "hex"));
}

async function main() {
  const provider = anchor.AnchorProvider.env();
  anchor.setProvider(provider);

  const program = anchor.workspace.SolanaPaPrototype as Program<SolanaPaPrototype>;

  const verifierRouter = requireVerifierRouter();
  const proofSelector = requireProofSelector();

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
  console.log(`  Verifier router: ${verifierRouter.toBase58()}`);
  console.log(`  Proof selector: 0x${Buffer.from(proofSelector).toString("hex")}`);

  await program.methods
    .initialize(verifierRouter, proofSelector)
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
