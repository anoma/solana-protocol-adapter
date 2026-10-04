import { AccountMeta, PublicKey } from "@solana/web3.js";
import {
  NULLIFIER_SEED,
  PA_STATE_SEED,
  ROOT_SEED,
  TX_DATA_SEED,
  UPGRADE_AUTHORITY_SEED,
} from "./constants";

export const BPF_LOADER_UPGRADEABLE = new PublicKey("BPFLoaderUpgradeab1e11111111111111111111111");

// Protocol adapter PDAs

export function derivePaStatePda(paProgramId: PublicKey): [PublicKey, number] {
  return PublicKey.findProgramAddressSync([PA_STATE_SEED], paProgramId);
}

/** The upgradeable loader's program-data account of a program. */
export function deriveProgramDataPda(programId: PublicKey): PublicKey {
  return PublicKey.findProgramAddressSync([programId.toBuffer()], BPF_LOADER_UPGRADEABLE)[0];
}

/** A program's upgrade authority once initialized: its PDA at `UPGRADE_AUTHORITY_SEED`, which only it signs for. */
export function deriveUpgradeAuthorityPda(programId: PublicKey): PublicKey {
  return PublicKey.findProgramAddressSync([UPGRADE_AUTHORITY_SEED], programId)[0];
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

/** A program's event authority PDA, the signer of its `#[event_cpi]` self-invocations. Seed: `["__event_authority"]`. */
export function deriveEventAuthorityPda(programId: PublicKey): [PublicKey, number] {
  return PublicKey.findProgramAddressSync([Buffer.from("__event_authority")], programId);
}
