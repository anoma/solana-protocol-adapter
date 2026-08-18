/**
 * Regenerate the preloaded mock VerifierEntry account fixture for the
 * current mock-verifier program ID.
 *
 * The risc0 verifier router dispatches a seal to the verifier registered
 * under the seal's selector via a VerifierEntry PDA. The devnet-cloned
 * router cannot register new verifiers locally (its initialize authority
 * is compile-time-baked into the immutable devnet binary), but the verify
 * path only reads the entry account — so the test validator preloads a
 * synthetic entry for the mock selector 0xffffffff at genesis, exactly
 * like the root-marker account fixtures. The entry embeds the
 * mock-verifier program ID, so rotating that ID strands the committed
 * fixture; this script regenerates it.
 *
 * Called by sync_program_ids (validator-deploy.sh) whenever the
 * mock-verifier program ID changes. Standalone:
 * npx ts-node -P tsconfig.json scripts/regen-mock-verifier-entry.ts
 */
import * as crypto from "crypto";
import { writeGenesisAccountFixtures } from "./genesis-account";
import {
  VERIFIER_ROUTER_ID,
  MOCK_VERIFIER_ID,
  MOCK_SELECTOR,
  getVerifierEntryPda,
} from "./verifier-utils";

// Anchor account layout: discriminator ‖ selector: [u8;4] ‖ verifier: Pubkey
// ‖ estopped: bool (risc0-solana verifier_router::state::VerifierEntry).
function verifierEntryData(): Buffer {
  const discriminator = crypto
    .createHash("sha256")
    .update("account:VerifierEntry")
    .digest()
    .subarray(0, 8);
  return Buffer.concat([
    discriminator,
    MOCK_SELECTOR,
    MOCK_VERIFIER_ID.toBuffer(),
    Buffer.from([0]), // estopped = false
  ]);
}

const [entryPda] = getVerifierEntryPda(MOCK_SELECTOR);
writeGenesisAccountFixtures({
  outDir: "tests/fixtures/verifier-entries",
  prefix: "verifier-entry-",
  owner: VERIFIER_ROUTER_ID,
  // 45-byte accounts need >= 1_204_080 lamports to be rent-exempt.
  lamports: 2_000_000,
  accounts: [{ pubkey: entryPda, data: verifierEntryData() }],
});
