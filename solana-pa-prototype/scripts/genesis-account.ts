/**
 * Shared writer for solana-test-validator genesis account fixtures — the
 * JSON files `--account <address> <file>` loads, named
 * `<prefix><address>.json` so start_validator can recover the address from
 * the filename. Used by the regen scripts for root markers and the mock
 * VerifierEntry.
 */
import * as fs from "fs";
import * as path from "path";
import { PublicKey } from "@solana/web3.js";

export function writeGenesisAccountFixtures(args: {
  outDir: string;
  prefix: string;
  owner: PublicKey;
  lamports: number;
  accounts: { pubkey: PublicKey; data: Buffer }[];
}): void {
  const { outDir, prefix, owner, lamports, accounts } = args;

  // Stale fixtures (from rotated program IDs) must go, not accumulate.
  for (const f of fs.readdirSync(outDir)) {
    if (f.startsWith(prefix) && f.endsWith(".json")) {
      fs.unlinkSync(path.join(outDir, f));
    }
  }

  for (const { pubkey, data } of accounts) {
    const account = {
      pubkey: pubkey.toBase58(),
      account: {
        lamports,
        data: [data.toString("base64"), "base64"],
        owner: owner.toBase58(),
        executable: false,
        // u64::MAX exceeds JS safe integers — placeholder swapped below so
        // the file carries the exact literal the validator expects.
        rentEpoch: "__RENT_EPOCH__",
        space: data.length,
      },
    };
    const outPath = path.join(outDir, `${prefix}${pubkey.toBase58()}.json`);
    const body = JSON.stringify(account, null, 2).replace('"__RENT_EPOCH__"', "18446744073709551615");
    fs.writeFileSync(outPath, body);
    console.log(`  wrote ${outPath} (owner ${owner.toBase58()})`);
  }
}
