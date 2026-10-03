/** `publish-idl.ts <idl json>`: `ops.sh idl-publish`'s writer (client/programMetadata.ts). */
import { publishIdl } from "../client/programMetadata";
import { fail, requireEnv } from "./cli-utils";

async function main() {
  const [idlPath] = process.argv.slice(2);
  if (!idlPath) fail("usage: publish-idl.ts <idl json>");
  const writer = await publishIdl(
    requireEnv("ANCHOR_PROVIDER_URL", "the RPC endpoint"),
    requireEnv("ANCHOR_WALLET", "the signing keypair file"),
    idlPath,
  );
  console.log(`✅ ${idlPath} written as the ${writer}; the cluster serves it.`);
}

main().catch((err) => {
  console.error("❌ publish-idl failed:", err);
  process.exit(1);
});
