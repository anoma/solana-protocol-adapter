import { AccountMeta, PublicKey } from "@solana/web3.js";
import {
  CONFIG_SEED,
  ESCROW_SEED,
  NONCE_BITMAP_SEED,
  NONCES_PER_WORD,
  NULLIFIER_SEED,
  PA_STATE_SEED,
  ROOT_SEED,
  TX_DATA_SEED,
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

/** A TxData upload account: one per (authority, upload id), the id as 8 little-endian bytes. */
export function deriveTxDataPda(paProgramId: PublicKey, authority: PublicKey, uploadIdLe: Buffer): PublicKey {
  return PublicKey.findProgramAddressSync([TX_DATA_SEED, authority.toBuffer(), uploadIdLe], paProgramId)[0];
}

export function deriveNullifierPda(programId: PublicKey, paState: PublicKey, nullifier: Buffer): [PublicKey, number] {
  return PublicKey.findProgramAddressSync([NULLIFIER_SEED, paState.toBuffer(), nullifier], programId);
}

export function deriveRootMarkerPda(paState: PublicKey, root: Buffer, programId: PublicKey): PublicKey {
  return PublicKey.findProgramAddressSync([ROOT_SEED, paState.toBuffer(), root], programId)[0];
}

/**
 * Derive nullifier marker account metas from base64-encoded nullifier bytes.
 * Returns writable, non-signer account metas suitable for remaining_accounts.
 */
export function deriveNullifierAccounts(
  nullifierB64s: string[],
  paState: PublicKey,
  programId: PublicKey,
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

/** The escrow authority: the one PDA that owns every mint's escrow token account. */
export function deriveEscrowAuthority(forwarderProgramId: PublicKey): PublicKey {
  return PublicKey.findProgramAddressSync([ESCROW_SEED], forwarderProgramId)[0];
}

/**
 * The escrow authority of `mint` under the forwarder's previous build, which
 * held each mint's escrow under its own `["escrow", mint]` authority;
 * `migrate_escrow` moves that escrow to the one authority above.
 */
export function derivePreviousEscrowAuthority(forwarderProgramId: PublicKey, mint: PublicKey): PublicKey {
  return PublicKey.findProgramAddressSync([ESCROW_SEED, mint.toBuffer()], forwarderProgramId)[0];
}

/** The 256-nonce word a nonce belongs to. */
export function nonceWordIndex(nonce: bigint): bigint {
  return nonce / NONCES_PER_WORD;
}

/** One bitmap account per (user, 256-nonce word), as the forwarder derives it. */
export function deriveNonceBitmapPda(
  forwarderProgramId: PublicKey,
  user: PublicKey,
  wordIndex: bigint,
): [PublicKey, number] {
  const word = Buffer.alloc(8);
  word.writeBigUInt64LE(wordIndex);
  return PublicKey.findProgramAddressSync([NONCE_BITMAP_SEED, user.toBuffer(), word], forwarderProgramId);
}

/** The adapter's event authority PDA, the signer of its `#[event_cpi]` self-invocations. Seed: `["__event_authority"]`. */
export function deriveEventAuthorityPda(paProgramId: PublicKey): [PublicKey, number] {
  return PublicKey.findProgramAddressSync([Buffer.from("__event_authority")], paProgramId);
}
