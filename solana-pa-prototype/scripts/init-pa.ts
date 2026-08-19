import * as anchor from "@coral-xyz/anchor";
import { Program } from "@coral-xyz/anchor";
import { PublicKey } from "@solana/web3.js";
import { ProtocolAdapter } from "../target/types/protocol_adapter";
import { PA_STATE_SEED } from "../tests/utils/constants";

const BPF_LOADER_UPGRADEABLE = new PublicKey(
  "BPFLoaderUpgradeab1e11111111111111111111111"
);

// `initialize` pins the router, selector, and kind-table commitment this
// deployment will trust for the lifetime of the PAState account. There is no
// safe default: guessing wrong installs the wrong verifier or rejects every
// settlement. All three must be supplied explicitly.
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

// Read a fixed-width hex byte string from a required environment variable.
// `missingHelp` says what the value is and why the script will not guess one.
function requireHexBytes(
  envVar: string,
  byteLen: number,
  missingHelp: string
): number[] {
  const raw = process.env[envVar];
  if (!raw) {
    console.error(`❌ Missing ${envVar}: ${missingHelp}`);
    process.exit(1);
  }
  const hex = raw.replace(/^0x/, "");
  if (hex.length !== byteLen * 2) {
    console.error(
      `❌ ${envVar} must be ${byteLen * 2} hex chars (${byteLen} bytes), got "${raw}"`
    );
    process.exit(1);
  }
  return Array.from(Buffer.from(hex, "hex"));
}

function requireProofSelector(): number[] {
  return requireHexBytes(
    "PA_PROOF_SELECTOR",
    4,
    "the 4-byte Groth16 verifier selector (hex, e.g. 0xdeadbeef) registered " +
      "with the verifier router for the circuit this deployment must accept.\n" +
      "   This script will not guess a default — initializing with the wrong " +
      "selector installs the wrong verifier."
  );
}

function requireKindTableCommitment(): number[] {
  return requireHexBytes(
    "PA_KIND_TABLE_COMMITMENT",
    32,
    "the sha256 commitment (hex, 32 bytes) of the kind table every settled " +
      "aggregation instance must carry.\n" +
      "   This script will not guess a default — initializing with the wrong " +
      "commitment rejects every settlement.\n" +
      "   For the empty table (fixture-gen's committed kind_table.json):\n" +
      "   e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
  );
}

async function main() {
  const provider = anchor.AnchorProvider.env();
  anchor.setProvider(provider);

  const program = anchor.workspace.ProtocolAdapter as Program<ProtocolAdapter>;

  const verifierRouter = requireVerifierRouter();
  const proofSelector = requireProofSelector();
  const kindTableCommitment = requireKindTableCommitment();

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
