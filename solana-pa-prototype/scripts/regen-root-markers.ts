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
import { PublicKey } from "@solana/web3.js";
import { PA_STATE_SEED } from "../tests/utils/constants";
import { deriveRootMarkerPda } from "../tests/utils/pda";
import { writeGenesisAccountFixtures } from "./genesis-account";

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

  writeGenesisAccountFixtures({
    outDir: OUT_DIR,
    prefix: "root-marker-",
    owner: programId,
    lamports: 1_000_000,
    accounts: roots.map((rootB64) => ({
      pubkey: deriveRootMarkerPda(paState, Buffer.from(rootB64, "base64"), programId),
      data: Buffer.alloc(0),
    })),
  });
}

main();
