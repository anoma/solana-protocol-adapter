/**
 * Verifier Router constants and PDA derivation utilities.
 *
 * These addresses are the RISC0 verifier infrastructure deployed on devnet
 * and cloned to localnet via Anchor.toml [test.validator.clone].
 */

import { PublicKey } from "@solana/web3.js";

// Devnet program addresses (cloned to localnet for testing)
export const VERIFIER_ROUTER_ID = new PublicKey("CnhgPbCm2mjYYT2konzKsBD7RL8Mfg63nuzB7xsbABFq");
export const GROTH16_VERIFIER_ID = new PublicKey("DBcDFEFD87rLdoepucSxbvG13idCo6HYS4sutVihkmbk");

/**
 * Derive the Router PDA (state account).
 * Seeds: ["router"]
 */
export function getRouterPda(routerProgramId: PublicKey = VERIFIER_ROUTER_ID): [PublicKey, number] {
  return PublicKey.findProgramAddressSync(
    [Buffer.from("router")],
    routerProgramId
  );
}

/**
 * Derive the Verifier Entry PDA for a given selector.
 * Seeds: ["verifier", selector]
 */
export function getVerifierEntryPda(
  selector: Buffer | Uint8Array,
  routerProgramId: PublicKey = VERIFIER_ROUTER_ID
): [PublicKey, number] {
  return PublicKey.findProgramAddressSync(
    [Buffer.from("verifier"), Buffer.from(selector)],
    routerProgramId
  );
}
