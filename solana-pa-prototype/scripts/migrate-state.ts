import * as anchor from "@anchor-lang/core";
import { confirmedProvider } from "../client/provider";
import { Program } from "@anchor-lang/core";
import { ProtocolAdapter } from "../target/types/protocol_adapter";
import { migrateState } from "../client/instructions";
import { PREVIOUS_SCHEMA_VERSION, SCHEMA_VERSION } from "../client/constants";
import { derivePaStatePda } from "../client/pda";

/**
 * `migrate_state` by the owner, once, right after the in-place upgrade to a
 * build whose state layout is the next schema version.
 */
async function main() {
  const provider = confirmedProvider();
  anchor.setProvider(provider);

  const program = anchor.workspace.ProtocolAdapter as Program<ProtocolAdapter>;
  const [paState] = derivePaStatePda(program.programId);
  const before = (await provider.connection.getAccountInfo(paState))?.data[8];
  if (before !== PREVIOUS_SCHEMA_VERSION) {
    throw new Error(`PAState is at schema version ${before}, not ${PREVIOUS_SCHEMA_VERSION}; nothing to migrate`);
  }

  await migrateState(program, provider.wallet.publicKey).rpc();

  const state = await program.account.paStateAccount.fetch(paState);
  if (state.schemaVersion !== SCHEMA_VERSION) {
    throw new Error(`PAState reads schema version ${state.schemaVersion} after the migration, not ${SCHEMA_VERSION}`);
  }
  console.log(`✅ Migrated PAState from schema version ${PREVIOUS_SCHEMA_VERSION} to ${SCHEMA_VERSION}`);
}

main().catch((err) => {
  console.error("❌ Migrating the state failed:", err.message || err);
  process.exit(1);
});
