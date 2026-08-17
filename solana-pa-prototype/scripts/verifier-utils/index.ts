// RISC0 verifier infrastructure deployed on devnet,
// cloned to localnet via Anchor.toml [test.validator.clone].

import { PublicKey } from "@solana/web3.js";

export const VERIFIER_ROUTER_ID = new PublicKey("BetEAE4npinksQBxvqUN1KkCVjYFJywWao45MSWtp5yg");
export const GROTH16_VERIFIER_ID = new PublicKey("2Yfa83Lzbn71ie3J1KQRiNQz1qHnvVm8gkBCpXZQ7ajD");

// Localnet-only mock verifier (programs/mock-verifier). ID synced by
// sync_program_ids in validator-deploy.sh from the committed keypair.
export const MOCK_VERIFIER_ID = new PublicKey("H3ZFoDHFvthGZu3kxpif3oSWm8MQn8uKvgDhrvVVHvHf");

// Selector the synthetic VerifierEntry registers the mock verifier under
// (risc0 fake-receipt convention; the real Groth16 selector is 0x73c457ba).
export const MOCK_SELECTOR = Buffer.from([0xff, 0xff, 0xff, 0xff]);

export function getRouterPda(routerProgramId: PublicKey = VERIFIER_ROUTER_ID): [PublicKey, number] {
  return PublicKey.findProgramAddressSync(
    [Buffer.from("router")],
    routerProgramId
  );
}

export function getVerifierEntryPda(
  selector: Buffer | Uint8Array,
  routerProgramId: PublicKey = VERIFIER_ROUTER_ID
): [PublicKey, number] {
  return PublicKey.findProgramAddressSync(
    [Buffer.from("verifier"), selector],
    routerProgramId
  );
}
