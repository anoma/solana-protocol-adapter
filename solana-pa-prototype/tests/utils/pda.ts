import { AccountMeta, PublicKey } from "@solana/web3.js";
import {
  CONFIG_SEED,
  NONCE_BITMAP_SEED,
  NONCES_PER_WORD,
  NULLIFIER_SEED,
  PA_STATE_SEED,
  ROOT_MARKER_SEED,
} from "./constants";

const BPF_LOADER_UPGRADEABLE = new PublicKey("BPFLoaderUpgradeab1e11111111111111111111111");

// Protocol adapter PDAs

export function derivePaStatePda(paProgramId: PublicKey): [PublicKey, number] {
  return PublicKey.findProgramAddressSync([PA_STATE_SEED], paProgramId);
}

/** The upgradeable loader's program-data account of a program. */
export function deriveProgramDataPda(programId: PublicKey): PublicKey {
  return PublicKey.findProgramAddressSync([programId.toBuffer()], BPF_LOADER_UPGRADEABLE)[0];
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

/**
 * Derive nullifier marker account metas from base64-encoded nullifier bytes.
 * Returns writable, non-signer account metas suitable for remaining_accounts.
 */
export function deriveNullifierAccounts(
  nullifierB64s: string[],
  paState: PublicKey,
  programId: PublicKey
): AccountMeta[] {
  return nullifierB64s.map((nfB64) => {
    const nf = Buffer.from(nfB64, "base64");
    const [pubkey] = deriveNullifierPda(programId, paState, nf);
    return { pubkey, isWritable: true, isSigner: false };
  });
}

// SPL token forwarder PDAs

export function deriveConfigPda(forwarderProgramId: PublicKey): [PublicKey, number] {
  return PublicKey.findProgramAddressSync([CONFIG_SEED], forwarderProgramId);
}

/** The 256-nonce word a nonce belongs to. */
export function nonceWordIndex(nonce: bigint): bigint {
  return nonce / NONCES_PER_WORD;
}

/** One bitmap account per (user, 256-nonce word), as the forwarder derives it. */
export function deriveNonceBitmapPda(
  forwarderProgramId: PublicKey,
  user: PublicKey,
  nonce: bigint
): [PublicKey, number] {
  const wordIndex = Buffer.alloc(8);
  wordIndex.writeBigUInt64LE(nonceWordIndex(nonce));
  return PublicKey.findProgramAddressSync(
    [NONCE_BITMAP_SEED, user.toBuffer(), wordIndex],
    forwarderProgramId
  );
}

/** The adapter's event authority PDA, the signer of its `#[event_cpi]` self-invocations. Seed: `["__event_authority"]`. */
export function deriveEventAuthorityPda(paProgramId: PublicKey): [PublicKey, number] {
  return PublicKey.findProgramAddressSync([Buffer.from("__event_authority")], paProgramId);
}
