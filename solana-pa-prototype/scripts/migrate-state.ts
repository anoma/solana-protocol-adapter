import * as anchor from "@anchor-lang/core";
import { Program } from "@anchor-lang/core";
import { ProtocolAdapter } from "../target/types/protocol_adapter";
import { migrateState } from "../client/instructions";
import { PREVIOUS_SCHEMA_VERSION, SCHEMA_VERSION } from "../client/constants";
import { derivePaStatePda } from "../client/pda";

async function main() {
  const provider = anchor.AnchorProvider.env();
  anchor.setProvider(provider);

  const program = anchor.workspace.ProtocolAdapter as Program<ProtocolAdapter>;
  const [paState] = derivePaStatePda(program.programId);

  const account = await provider.connection.getAccountInfo(paState);
  if (!account) throw new Error(`PAState ${paState.toBase58()} does not exist`);
  // Byte 8, after Anchor's discriminator, is the schema version in every layout.
  const version = account.data[8];
  if (version === SCHEMA_VERSION) {
    console.log(`PAState ${paState.toBase58()} is already at schema version ${SCHEMA_VERSION}`);
    return;
  }
  if (version !== PREVIOUS_SCHEMA_VERSION) {
    throw new Error(
      `PAState is at schema version ${version}; this build migrates only from ${PREVIOUS_SCHEMA_VERSION}`,
    );
  }

  await migrateState(program, provider.wallet.publicKey).rpc();

  console.log(`✅ Migrated PAState ${paState.toBase58()} from schema ${version} to ${SCHEMA_VERSION}`);
}

main().catch((err) => {
  console.error("❌ Migrating the state failed:", err.message || err);
  process.exit(1);
});
