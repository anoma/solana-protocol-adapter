/**
 * Regenerate the preloaded root-marker account fixtures for the current PA
 * program ID.
 *
 * The imported AnomaPay transfer fixture consumes resources proven against
 * historical roots of an external deployment's tree. The local test
 * validator preloads marker accounts for those roots
 * (tests/fixtures/anomapay-root-markers/) — but a marker's ADDRESS and
 * OWNER both derive from the PA program ID, so rotating the program ID
 * silently strands the committed account fixtures at the old addresses.
 * The transaction fixtures themselves bind only the forwarder ID and stay
 * valid; only these account fixtures need regeneration. The roots are
 * plain data carried in the fixture's historical_roots_b64, so this needs
 * no proving.
 *
 * Called by sync_program_ids (validator-deploy.sh) whenever the PA program
 * ID changes. Standalone: npx ts-node -P tsconfig.json scripts/regen-root-markers.ts
 */
import * as fs from "fs";
import * as path from "path";
import { PublicKey } from "@solana/web3.js";
import { PA_STATE_SEED } from "../tests/utils/constants";
import { deriveRootMarkerPda } from "../tests/utils/pda";

const FIXTURE = "tests/fixtures/anomapay_transfer_0e345103.json";
const OUT_DIR = "tests/fixtures/anomapay-root-markers";
const ANCHOR_TOML = "Anchor.toml";

function currentPaProgramId(): PublicKey {
  // Anchor.toml is the synced source get_program_id also agrees with;
  // reading it avoids requiring a built keypair or IDL.
  const toml = fs.readFileSync(ANCHOR_TOML, "utf-8");
  const m = toml.match(/^protocol_adapter = "([1-9A-HJ-NP-Za-km-z]+)"$/m);
  if (!m) {
    console.error(`❌ Could not read protocol_adapter ID from ${ANCHOR_TOML}`);
    process.exit(1);
  }
  return new PublicKey(m[1]);
}

function main() {
  const programId = currentPaProgramId();
  const [paState] = PublicKey.findProgramAddressSync([PA_STATE_SEED], programId);

  const fixture = JSON.parse(fs.readFileSync(FIXTURE, "utf-8"));
  const roots: string[] = fixture.historical_roots_b64;
  if (!Array.isArray(roots) || roots.length === 0) {
    console.error(`❌ ${FIXTURE} has no historical_roots_b64 — nothing to derive from`);
    process.exit(1);
  }

  // Stale markers (old program ID) must go, not accumulate.
  for (const f of fs.readdirSync(OUT_DIR)) {
    if (f.startsWith("root-marker-") && f.endsWith(".json")) {
      fs.unlinkSync(path.join(OUT_DIR, f));
    }
  }

  for (const rootB64 of roots) {
    const root = Buffer.from(rootB64, "base64");
    const marker = deriveRootMarkerPda(paState, root, programId);
    const account = {
      pubkey: marker.toBase58(),
      account: {
        lamports: 1_000_000,
        data: ["", "base64"],
        owner: programId.toBase58(),
        executable: false,
        // u64::MAX exceeds JS safe integers — placeholder swapped below so
        // the file carries the exact literal the validator expects.
        rentEpoch: "__RENT_EPOCH__",
        space: 0,
      },
    };
    const outPath = path.join(OUT_DIR, `root-marker-${marker.toBase58()}.json`);
    const body = JSON.stringify(account, null, 2).replace(
      '"__RENT_EPOCH__"',
      "18446744073709551615"
    );
    fs.writeFileSync(outPath, body);
    console.log(`  wrote ${outPath} (owner ${programId.toBase58()})`);
  }
}

main();
