import * as anchor from "@anchor-lang/core";
import { Program } from "@anchor-lang/core";
import { ProtocolAdapter } from "../target/types/protocol_adapter";
import { setKindTableCommitment } from "../client/instructions";
import { requireHexBytes } from "./cli-utils";

async function main() {
  const provider = anchor.AnchorProvider.env();
  anchor.setProvider(provider);

  const program = anchor.workspace.ProtocolAdapter as Program<ProtocolAdapter>;

  const kindTableCommitment = requireHexBytes(
    "PA_KIND_TABLE_COMMITMENT",
    32,
    "the sha256 commitment (hex, 32 bytes) of the kind table every settled aggregation instance must carry from now on.\n" +
      "   Transactions proven against the previous table are rejected once this is installed.",
  );

  await setKindTableCommitment(program, provider.wallet.publicKey, kindTableCommitment).rpc();

  console.log(`✅ Kind table commitment: ${Buffer.from(kindTableCommitment).toString("hex")}`);
}

main().catch((err) => {
  console.error("❌ Setting the kind table commitment failed:", err.message || err);
  process.exit(1);
});
