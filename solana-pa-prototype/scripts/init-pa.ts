import * as anchor from "@anchor-lang/core";
import { Program } from "@anchor-lang/core";
import { ProtocolAdapter } from "../target/types/protocol_adapter";
import { initializeAdapter } from "../tests/utils/helpers";
import { derivePaStatePda } from "../tests/utils/pda";
import { requireHexBytes, requirePubkey } from "./cli-utils";

async function main() {
  const provider = anchor.AnchorProvider.env();
  anchor.setProvider(provider);

  const program = anchor.workspace.ProtocolAdapter as Program<ProtocolAdapter>;

  // `initialize` pins the router and selector this deployment will trust for
  // the lifetime of the PAState account. There is no safe default: guessing
  // wrong installs the wrong verifier. Both must be supplied explicitly. The
  // adapter starts on the empty kind table; set-kind-table installs another.
  const verifierRouter = requirePubkey(
    "PA_VERIFIER_ROUTER",
    "the RISC0 verifier router program ID this deployment must trust, as a base58 pubkey.\n" +
      "   This script will not guess a default — initializing against the wrong router installs the wrong verifier.",
  );
  const proofSelector = requireHexBytes(
    "PA_PROOF_SELECTOR",
    4,
    "the 4-byte Groth16 verifier selector (hex, e.g. 0xdeadbeef) registered with the verifier router " +
      "for the circuit this deployment must accept.\n" +
      "   This script will not guess a default — initializing with the wrong selector installs the wrong verifier.",
  );

  const [paState] = derivePaStatePda(program.programId);

  // Idempotent: skip if PAState already exists.
  if (await program.account.paStateAccount.fetchNullable(paState)) {
    console.log("PAState already initialized, skipping.");
    return;
  }

  console.log("Initializing PA...");
  console.log(`  PAState PDA: ${paState.toBase58()}`);
  console.log(`  Verifier router: ${verifierRouter.toBase58()}`);
  console.log(`  Proof selector: 0x${Buffer.from(proofSelector).toString("hex")}`);

  await initializeAdapter(program, provider.wallet.publicKey, verifierRouter, proofSelector).rpc();

  console.log("✅ PA initialized");
}

main().catch((err) => {
  console.error("❌ PA initialization failed:", err.message || err);
  process.exit(1);
});
