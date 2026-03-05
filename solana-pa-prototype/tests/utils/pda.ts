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

export function deriveRootMarkerPda(
  paState: PublicKey,
  root: Buffer,
  paProgramId: PublicKey
): PublicKey {
  return PublicKey.findProgramAddressSync(
    [ROOT_MARKER_SEED, paState.toBuffer(), root],
    paProgramId
  )[0];
}

export function deriveNullifierPda(
  paState: PublicKey,
  nullifier: Buffer,
  paProgramId: PublicKey
): PublicKey {
  return PublicKey.findProgramAddressSync(
    [NULLIFIER_SEED, paState.toBuffer(), nullifier],
    paProgramId
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

export function parseSelectorFromFixture(selectorHex: string): Buffer {
  const hex = selectorHex.replace(/^0x/, "");
  if (hex.length !== 8) {
    throw new Error(
      `Invalid selector format: ${selectorHex} (expected 8 hex chars)`
    );
  }
  return Buffer.from(hex, "hex");
}

export function deriveRouterAccounts(
  verifierRouterId: PublicKey,
  selector: Buffer
): { routerPda: PublicKey; verifierEntryPda: PublicKey } {
  const [routerPda] = getRouterPda(verifierRouterId);
  const [verifierEntryPda] = getVerifierEntryPda(selector, verifierRouterId);
  return { routerPda, verifierEntryPda };
}
