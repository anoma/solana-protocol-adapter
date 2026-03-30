import { PublicKey } from "@solana/web3.js";
import { PA_STATE_SEED, NULLIFIER_SEED, ROOT_MARKER_SEED } from "./constants";

export function derivePaStatePda(programId: PublicKey): [PublicKey, number] {
  return PublicKey.findProgramAddressSync([PA_STATE_SEED], programId);
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
