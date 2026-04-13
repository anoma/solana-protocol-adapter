import { PublicKey } from "@solana/web3.js";
import {
  getRouterPda,
  getVerifierEntryPda,
} from "../../scripts/verifier-utils";
import {
  PA_STATE_SEED,
  NULLIFIER_SEED,
  TX_DATA_SEED,
  ROOT_MARKER_SEED,
  CONFIG_SEED,
  ESCROW_SEED,
  NONCE_BITMAP_SEED,
  NONCES_PER_WORD,
} from "./constants";

// PA PDAs

export function derivePaStatePda(paProgramId: PublicKey): [PublicKey, number] {
  return PublicKey.findProgramAddressSync([PA_STATE_SEED], paProgramId);
}

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

export function deriveTxDataPda(
  authority: PublicKey,
  uploadIdLe: Buffer,
  paProgramId: PublicKey
): PublicKey {
  return PublicKey.findProgramAddressSync(
    [TX_DATA_SEED, authority.toBuffer(), uploadIdLe],
    paProgramId
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

// SPL Token Forwarder PDAs

export function deriveConfigPda(programId: PublicKey): [PublicKey, number] {
  return PublicKey.findProgramAddressSync([CONFIG_SEED], programId);
}

export function deriveEscrowPda(
  programId: PublicKey,
  tokenMint: PublicKey
): [PublicKey, number] {
  return PublicKey.findProgramAddressSync(
    [ESCROW_SEED, tokenMint.toBuffer()],
    programId
  );
}

export function deriveNonceBitmapPda(
  programId: PublicKey,
  user: PublicKey,
  nonce: bigint
): [PublicKey, number] {
  const wordIndex = nonce / NONCES_PER_WORD;
  const wordIndexBuffer = Buffer.alloc(8);
  wordIndexBuffer.writeBigUInt64LE(wordIndex);
  return PublicKey.findProgramAddressSync(
    [NONCE_BITMAP_SEED, user.toBuffer(), wordIndexBuffer],
    programId
  );
}

// Verifier router PDAs

export function deriveRouterAccounts(
  verifierRouterId: PublicKey,
  selector: Buffer
): { routerPda: PublicKey; verifierEntryPda: PublicKey } {
  const [routerPda] = getRouterPda(verifierRouterId);
  const [verifierEntryPda] = getVerifierEntryPda(selector, verifierRouterId);
  return { routerPda, verifierEntryPda };
}
