import * as anchor from "@anchor-lang/core";
import { confirmedProvider } from "../client/provider";
import { Program } from "@anchor-lang/core";
import { ProtocolAdapter } from "../target/types/protocol_adapter";
import { migrateState } from "../client/instructions";
import { PREVIOUS_SCHEMA_VERSION, SCHEMA_VERSION } from "../client/constants";

/**
 * `migrate_state` by the owner, once, right after the in-place upgrade to a
 * build whose state layout is the next schema version. The program refuses
 * a state account in any other version (`NotPreviousSchema`).
 */
async function main() {
  const provider = confirmedProvider();
  anchor.setProvider(provider);

  const program = anchor.workspace.ProtocolAdapter as Program<ProtocolAdapter>;
  await migrateState(program, provider.wallet.publicKey).rpc();

  console.log(`✅ Migrated PAState from schema version ${PREVIOUS_SCHEMA_VERSION} to ${SCHEMA_VERSION}`);
}

main().catch((err) => {
  console.error("❌ Migrating the state failed:", err.message || err);
  process.exit(1);
});
