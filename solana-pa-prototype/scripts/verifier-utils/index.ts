// RISC0 verifier infrastructure deployed on devnet,
// cloned to localnet via Anchor.toml [test.validator.clone].

import { PublicKey } from "@solana/web3.js";

export const VERIFIER_ROUTER_ID = new PublicKey("Gh6PcNHqUjCzycGYbCUnjELthWgiCJSJD7GHMXWM5wUJ");
export const GROTH16_VERIFIER_ID = new PublicKey("AZBNqidSdMKPtrvXaZrqTkFoCJfmkJPQjjvE84Je4mX8");

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
