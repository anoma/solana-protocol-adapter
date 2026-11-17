import * as anchor from "@anchor-lang/core";
import { confirmedProvider } from "../client/provider";
import { Program } from "@anchor-lang/core";
import { ProtocolAdapter } from "../target/types/protocol_adapter";
import { initializeAdapter } from "../client/instructions";
import { derivePaStatePda } from "../client/pda";
import { requirePubkey } from "./cli-utils";

async function main() {
  const provider = confirmedProvider();
  anchor.setProvider(provider);

  const program = anchor.workspace.ProtocolAdapter as Program<ProtocolAdapter>;

  // `initialize` sets the owner, as pa-evm's initializer does; the verifier
  // router and selector are the build's (env/<cluster>.env), as pa-evm's
  // implementation holds them. There is no safe default owner: guessing
  // wrong hands the adapter to the wrong key. The signer, the program's
  // upgrade authority, hands that authority to the program. The adapter
  // starts on the empty kind tree; set-kind-tree-root installs another.
  const owner = requirePubkey(
    "PA_OWNER",
    "the adapter's initial owner, who alone pauses, upgrades and configures it, as a base58 pubkey.\n" +
      "   This script will not guess a default — initializing with the wrong owner hands the adapter to that key.",
  );

  const [paState] = derivePaStatePda(program.programId);

  // Idempotent: skip if PAState already exists.
  if (await program.account.paStateAccount.fetchNullable(paState)) {
    console.log("PAState already initialized, skipping.");
    return;
  }

  console.log("Initializing PA...");
  console.log(`  PAState PDA: ${paState.toBase58()}`);
  console.log(`  Owner: ${owner.toBase58()}`);

  await initializeAdapter(program, provider.wallet.publicKey, owner).rpc();

  console.log("✅ PA initialized");
}

main().catch((err) => {
  console.error("❌ PA initialization failed:", err.message || err);
  process.exit(1);
});
