// RISC0 verifier infrastructure deployed on devnet,
// cloned to localnet via Anchor.toml [test.validator.clone].

import * as anchor from "@anchor-lang/core";
import { Program } from "@anchor-lang/core";
import { PublicKey } from "@solana/web3.js";
import { MockVerifier } from "../../target/types/mock_verifier";

export const VERIFIER_ROUTER_ID = new PublicKey("BetEAE4npinksQBxvqUN1KkCVjYFJywWao45MSWtp5yg");
export const GROTH16_VERIFIER_ID = new PublicKey("2Yfa83Lzbn71ie3J1KQRiNQz1qHnvVm8gkBCpXZQ7ajD");

// Selector the synthetic VerifierEntry registers the mock verifier under
// (risc0 fake-receipt convention; the real Groth16 selector is 0x73c457ba).
export const MOCK_SELECTOR = Buffer.from([0xff, 0xff, 0xff, 0xff]);

// Verifiers by router selector. Fixtures carry their selector, so tests
// derive the verifier program and its error codes from the fixture instead
// of hardcoding one. `rejectionCode` rejects a well-formed proof that does not
// verify; `malformedProofCode` rejects proof bytes that are not valid curve
// points. Unknown selectors fail loudly rather than silently defaulting to
// some verifier.
type Verifier = { program: PublicKey; rejectionCode: number; malformedProofCode: number };

// The localnet-only mock verifier (programs/mock-verifier) is resolved
// through the Anchor workspace on use, so importing this module needs no
// workspace or provider.
export function verifierForSelector(selector: Buffer): Verifier {
  const hex = selector.toString("hex");
  switch (hex) {
    // groth_16_verifier: VerificationError, PairingError
    case "73c457ba":
      return { program: GROTH16_VERIFIER_ID, rejectionCode: 6000, malformedProofCode: 6003 };
    // mock-verifier: ClaimDigestMismatch for both (it checks no curve points;
    // offset 6600 keeps it disjoint)
    case "ffffffff":
      return {
        program: (anchor.workspace.MockVerifier as Program<MockVerifier>).programId,
        rejectionCode: 6600,
        malformedProofCode: 6600,
      };
    default:
      throw new Error(`no verifier registered for selector 0x${hex}`);
  }
}

export function getRouterPda(routerProgramId: PublicKey = VERIFIER_ROUTER_ID): [PublicKey, number] {
  return PublicKey.findProgramAddressSync([Buffer.from("router")], routerProgramId);
}

export function getVerifierEntryPda(
  selector: Buffer | Uint8Array,
  routerProgramId: PublicKey = VERIFIER_ROUTER_ID,
): [PublicKey, number] {
  return PublicKey.findProgramAddressSync([Buffer.from("verifier"), selector], routerProgramId);
}

/**
 * The verifier program a router `VerifierEntry` account points at: Anchor
 * discriminator ‖ selector [u8; 4] ‖ verifier Pubkey ‖ estopped bool (the
 * layout `regen-mock-verifier-entry.ts` encodes).
 */
export function verifierOfEntry(data: Buffer): PublicKey {
  return new PublicKey(data.subarray(12, 44));
}
