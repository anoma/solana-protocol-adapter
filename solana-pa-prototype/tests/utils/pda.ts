import { PublicKey } from "@solana/web3.js";
import { NULLIFIER_SEED, ROOT_MARKER_SEED } from "./constants";

export function deriveNullifierPda(
  programId: PublicKey,
  paState: PublicKey,
  nullifier: Buffer
): [PublicKey, number] {
  return PublicKey.findProgramAddressSync(
    [NULLIFIER_SEED, paState.toBuffer(), nullifier],
    programId
  );
}

export function deriveRootMarkerPda(
  paState: PublicKey,
  root: Buffer,
  programId: PublicKey
): PublicKey {
  return PublicKey.findProgramAddressSync(
    [ROOT_MARKER_SEED, paState.toBuffer(), root],
    programId
  )[0];
}

/**
 * Derive nullifier marker account metas from base64-encoded nullifier bytes.
 * Returns writable, non-signer account metas suitable for remaining_accounts.
 */
export function deriveNullifierAccounts(
  nullifierB64s: string[],
  paState: PublicKey,
  programId: PublicKey
): { pubkey: PublicKey; isWritable: boolean; isSigner: boolean }[] {
  return nullifierB64s.map((nfB64) => {
    const nf = Buffer.from(nfB64, "base64");
    const [pubkey] = deriveNullifierPda(programId, paState, nf);
    return { pubkey, isWritable: true, isSigner: false };
  });
}
