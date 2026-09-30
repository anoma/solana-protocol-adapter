// RISC0 verifier infrastructure deployed on devnet, copied into each local
// validator's genesis (validator-deploy.sh: fetch_devnet_clones, start_validator).

import { PublicKey } from "@solana/web3.js";

export const VERIFIER_ROUTER_ID = new PublicKey("BetEAE4npinksQBxvqUN1KkCVjYFJywWao45MSWtp5yg");
export const GROTH16_VERIFIER_ID = new PublicKey("2Yfa83Lzbn71ie3J1KQRiNQz1qHnvVm8gkBCpXZQ7ajD");

// Selector the synthetic VerifierEntry registers the mock verifier under
// (risc0 fake-receipt convention; the real Groth16 selector is 0x73c457ba).
export const MOCK_SELECTOR = Buffer.from([0xff, 0xff, 0xff, 0xff]);
/** Localnet only: a selector registered to the mock verifier with the router's estop set, a paused verifier. */
export const PAUSED_MOCK_SELECTOR = Buffer.from([0xff, 0xff, 0xff, 0xfe]);

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
