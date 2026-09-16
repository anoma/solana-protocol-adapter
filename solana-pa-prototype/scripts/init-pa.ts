import * as anchor from "@coral-xyz/anchor";
import { Program } from "@coral-xyz/anchor";
import { PublicKey } from "@solana/web3.js";
import { ProtocolAdapter } from "../target/types/protocol_adapter";
import { derivePaStatePda, deriveProgramDataPda } from "../tests/utils/pda";
import { requireHexBytes, requirePubkey } from "./cli-utils";

async function main() {
  const provider = anchor.AnchorProvider.env();
  anchor.setProvider(provider);

  const program = anchor.workspace.ProtocolAdapter as Program<ProtocolAdapter>;

  // `initialize` pins the router, selector, and kind-table commitment this
  // deployment will trust for the lifetime of the PAState account. There is
  // no safe default: guessing wrong installs the wrong verifier or rejects
  // every settlement. All three must be supplied explicitly.
  const verifierRouter = requirePubkey(
    "PA_VERIFIER_ROUTER",
    "the RISC0 verifier router program ID this deployment must trust, as a base58 pubkey.\n" +
      "   This script will not guess a default — initializing against the wrong router installs the wrong verifier."
  );
  const proofSelector = requireHexBytes(
    "PA_PROOF_SELECTOR",
    4,
    "the 4-byte Groth16 verifier selector (hex, e.g. 0xdeadbeef) registered with the verifier router " +
      "for the circuit this deployment must accept.\n" +
      "   This script will not guess a default — initializing with the wrong selector installs the wrong verifier."
  );
  const kindTableCommitment = requireHexBytes(
    "PA_KIND_TABLE_COMMITMENT",
    32,
    "the sha256 commitment (hex, 32 bytes) of the kind table every settled aggregation instance must carry.\n" +
      "   This script will not guess a default — initializing with the wrong commitment rejects every settlement.\n" +
      "   For the empty table (fixture-gen's committed kind_table.json):\n" +
      "   e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
  );

  const [paState] = derivePaStatePda(program.programId);
  const programData = deriveProgramDataPda(program.programId);

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
  console.log(
    `  Kind table commitment: ${Buffer.from(kindTableCommitment).toString("hex")}`
  );

  await program.methods
    .initialize(verifierRouter, proofSelector, kindTableCommitment)
    .accountsPartial({
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
