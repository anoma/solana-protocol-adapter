import * as anchor from "@anchor-lang/core";
import { confirmedProvider } from "../client/provider";
import { Program } from "@anchor-lang/core";
import { ProtocolAdapter } from "../target/types/protocol_adapter";
import { DeniedLogicRef, denyLogicRefs } from "../client/instructions";
import { requireEnv } from "./cli-utils";

/**
 * PA_DENIED_LOGIC_REFS, comma-separated `<hex logic ref>:<consumed|created>`
 * entries, as pa-evm's ExecuteLogicRefDenial takes `(logicRef, consumed)`
 * pairs: `created` alone deprecates a logic ref, both deny it.
 */
function requireDeniedLogicRefs(): DeniedLogicRef[] {
  const raw = requireEnv(
    "PA_DENIED_LOGIC_REFS",
    "comma-separated <logic ref, hex, 32 bytes>:<consumed|created> entries, each adding the logic ref to the\n" +
      "   denylist for consumed or for created resources. An entry cannot be removed.",
  );
  return raw.split(",").map((entry) => {
    const match = /^(?:0x)?([0-9a-fA-F]{64}):(consumed|created)$/.exec(entry.trim());
    if (!match) throw new Error(`PA_DENIED_LOGIC_REFS entry must be <64 hex chars>:<consumed|created>, got "${entry}"`);
    return { logicRef: Array.from(Buffer.from(match[1], "hex")), consumed: match[2] === "consumed" };
  });
}

async function main() {
  const provider = confirmedProvider();
  anchor.setProvider(provider);

  const program = anchor.workspace.ProtocolAdapter as Program<ProtocolAdapter>;
  const logicRefs = requireDeniedLogicRefs();

  await denyLogicRefs(program, provider.wallet.publicKey, logicRefs).rpc();

  for (const { logicRef, consumed } of logicRefs) {
    console.log(
      `✅ Denied logic ref ${Buffer.from(logicRef).toString("hex")} for ${consumed ? "consumed" : "created"} resources`,
    );
  }
}

main().catch((err) => {
  console.error("❌ Denying the logic refs failed:", err.message || err);
  process.exit(1);
});
