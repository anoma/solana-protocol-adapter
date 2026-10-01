/**
 * Instruction builders for the protocol adapter and the SPL token forwarder:
 * the one form of each instruction the operator scripts and the test suite
 * both send. Builders return an Anchor method builder; callers add signers
 * and send.
 */
import { BN, Program } from "@anchor-lang/core";
import { AccountMeta, Keypair, PublicKey, SystemProgram, SYSVAR_INSTRUCTIONS_PUBKEY } from "@solana/web3.js";
import { getAssociatedTokenAddressSync, TOKEN_PROGRAM_ID } from "@solana/spl-token";
import { ProtocolAdapter } from "../target/types/protocol_adapter";
import { SplTokenForwarder } from "../target/types/spl_token_forwarder";
import {
  deriveConfigPda,
  deriveEscrowAuthority,
  derivePaStatePda,
  derivePreviousEscrowAuthority,
  deriveProgramDataPda,
} from "./pda";

// Protocol adapter governance

/** The adapter's `initialize`, paid by `payer`, pinning the verifier; it starts on the empty kind table. */
export function initializeAdapter(
  program: Program<ProtocolAdapter>,
  payer: PublicKey,
  verifierRouter: PublicKey,
  proofSelector: number[],
) {
  return program.methods.initialize(verifierRouter, proofSelector).accountsPartial({
    paState: derivePaStatePda(program.programId)[0],
    payer,
    systemProgram: SystemProgram.programId,
  });
}

// The adapter's owner is the program's upgrade authority: every builder
// below takes it as `authority`, and Anchor resolves the program's
// ProgramData, where the loader records it.

/** `deny_logic_ref` by the upgrade authority, which pays for the entry. */
export function denyLogicRef(program: Program<ProtocolAdapter>, authority: PublicKey, logicRef: number[]) {
  return program.methods
    .denyLogicRef(logicRef)
    .accountsPartial({ paState: derivePaStatePda(program.programId)[0], authority });
}

/**
 * `migrate_state` by the program's upgrade authority: brings a state account
 * in the previous schema version to this build's, once, after an in-place
 * upgrade.
 */
export function migrateState(program: Program<ProtocolAdapter>, authority: PublicKey) {
  return program.methods.migrateState().accounts({ authority });
}

/** `emergency_stop` by the upgrade authority. */
export function emergencyStop(program: Program<ProtocolAdapter>, authority: PublicKey) {
  return program.methods
    .emergencyStop()
    .accountsPartial({ paState: derivePaStatePda(program.programId)[0], authority });
}

/** `set_kind_table_commitment` by the upgrade authority. */
export function setKindTableCommitment(program: Program<ProtocolAdapter>, authority: PublicKey, commitment: number[]) {
  return program.methods
    .setKindTableCommitment(commitment)
    .accountsPartial({ paState: derivePaStatePda(program.programId)[0], authority });
}

// SPL token forwarder

/**
 * The 72-byte unwrap operand (token_mint, amount u64 LE, recipient): the
 * whole input of forward_emergency_call, and forward_call's input after
 * the op-code byte.
 */
export function encodeUnwrapInput(tokenMint: PublicKey, amount: bigint, recipient: PublicKey): Buffer {
  const operand = Buffer.alloc(72);
  tokenMint.toBuffer().copy(operand, 0);
  operand.writeBigUInt64LE(amount, 32);
  recipient.toBuffer().copy(operand, 40);
  return operand;
}

/** A mint's escrow: the forwarder's escrow authority and its associated token account for the mint. */
export function escrowAccounts(
  forwarderProgramId: PublicKey,
  mint: PublicKey,
): { escrowAuthority: PublicKey; escrowAta: PublicKey } {
  const escrowAuthority = deriveEscrowAuthority(forwarderProgramId);
  return { escrowAuthority, escrowAta: getAssociatedTokenAddressSync(mint, escrowAuthority, true) };
}

/** A mint's escrow under the previous build: its per-mint escrow authority and that authority's token account. */
export function previousEscrowAccounts(
  forwarderProgramId: PublicKey,
  mint: PublicKey,
): { previousEscrowAuthority: PublicKey; previousEscrowAta: PublicKey } {
  const previousEscrowAuthority = derivePreviousEscrowAuthority(forwarderProgramId, mint);
  return {
    previousEscrowAuthority,
    previousEscrowAta: getAssociatedTokenAddressSync(mint, previousEscrowAuthority, true),
  };
}

/** The head of every forwarder call segment: the forwarder, its config and the instructions sysvar. */
export function forwarderSegmentHead(forwarderProgramId: PublicKey): AccountMeta[] {
  return [
    { pubkey: forwarderProgramId, isSigner: false, isWritable: false },
    { pubkey: deriveConfigPda(forwarderProgramId)[0], isSigner: false, isWritable: false },
    { pubkey: SYSVAR_INSTRUCTIONS_PUBKEY, isSigner: false, isWritable: false },
  ];
}

/**
 * The accounts of a wrap, in the order the program reads them: the wrap's
 * remaining accounts after the segment head.
 */
export function wrapTransferAccounts(
  source: PublicKey,
  destination: PublicKey,
  escrowAuthority: PublicKey,
  nonceBitmap: PublicKey,
): AccountMeta[] {
  return [
    { pubkey: source, isSigner: false, isWritable: true },
    { pubkey: destination, isSigner: false, isWritable: true },
    { pubkey: escrowAuthority, isSigner: false, isWritable: false },
    { pubkey: nonceBitmap, isSigner: false, isWritable: true },
    { pubkey: TOKEN_PROGRAM_ID, isSigner: false, isWritable: false },
  ];
}

/**
 * The accounts of an escrow release, in the order the program reads them:
 * the unwrap's remaining accounts after the segment head, and the whole of
 * forward_emergency_call's.
 */
export function escrowTransferAccounts(
  escrowAta: PublicKey,
  recipientAta: PublicKey,
  escrowAuthority: PublicKey,
): AccountMeta[] {
  return [
    { pubkey: escrowAta, isSigner: false, isWritable: true },
    { pubkey: recipientAta, isSigner: false, isWritable: true },
    { pubkey: escrowAuthority, isSigner: false, isWritable: false },
    { pubkey: TOKEN_PROGRAM_ID, isSigner: false, isWritable: false },
  ];
}

/** The forwarder's `initialize`; callers add signers and send. */
export function initializeForwarder(
  forwarder: Program<SplTokenForwarder>,
  adapterProgramId: PublicKey,
  logicRef: number[],
  committee: PublicKey,
  authority: PublicKey,
  programData: PublicKey = deriveProgramDataPda(forwarder.programId),
) {
  return forwarder.methods.initialize(adapterProgramId, logicRef, committee).accounts({ authority, programData });
}

/**
 * Rotate the forwarder config's logic ref, once per build that raises
 * CONFIG_VERSION, after upgrading the program to that build. `authority`
 * must be the program's upgrade authority; `programData` is the forwarder's
 * own ProgramData unless a test substitutes another program's.
 */
export function reinitializeForwarder(
  forwarder: Program<SplTokenForwarder>,
  authority: PublicKey,
  logicRef: number[],
  programData: PublicKey = deriveProgramDataPda(forwarder.programId),
) {
  return forwarder.methods.reinitialize(logicRef).accounts({ authority, programData });
}

/**
 * The migrations that bring the accounts of the forwarder's previous build
 * to this build's layout, run once by the upgrade authority after upgrading
 * the program in place.
 */
export function migrateConfig(forwarder: Program<SplTokenForwarder>, authority: PublicKey) {
  return forwarder.methods
    .migrateConfig()
    .accounts({ authority, programData: deriveProgramDataPda(forwarder.programId) });
}

/** Migrate `user`'s nonce bitmap for the 256-nonce word `wordIndex`. */
export function migrateNonceBitmap(
  forwarder: Program<SplTokenForwarder>,
  authority: PublicKey,
  user: PublicKey,
  wordIndex: bigint,
) {
  return forwarder.methods
    .migrateNonceBitmap(user, new BN(wordIndex.toString()))
    .accounts({ authority, programData: deriveProgramDataPda(forwarder.programId) });
}

/** Move `mint`'s escrow from its previous per-mint authority to the escrow authority. */
export function migrateEscrow(forwarder: Program<SplTokenForwarder>, authority: PublicKey, mint: PublicKey) {
  return forwarder.methods.migrateEscrow().accounts({
    authority,
    tokenMint: mint,
    previousEscrowAta: previousEscrowAccounts(forwarder.programId, mint).previousEscrowAta,
    escrowAta: escrowAccounts(forwarder.programId, mint).escrowAta,
    programData: deriveProgramDataPda(forwarder.programId),
  });
}

/** `forward_emergency_call` by `caller`; callers add signers and send. */
export function emergencyWithdraw(
  forwarder: Program<SplTokenForwarder>,
  paState: PublicKey,
  caller: PublicKey,
  withdrawal: { mint: PublicKey; amount: bigint; recipient: PublicKey },
  accounts: { escrowAta: PublicKey; recipientAta: PublicKey },
) {
  return forwarder.methods
    .forwardEmergencyCall(encodeUnwrapInput(withdrawal.mint, withdrawal.amount, withdrawal.recipient))
    .accounts({ caller, paState })
    .remainingAccounts(
      escrowTransferAccounts(accounts.escrowAta, accounts.recipientAta, deriveEscrowAuthority(forwarder.programId)),
    );
}

/**
 * `close_escrow` by `authority`, draining the escrow to `recipientAta`;
 * requires the adapter at `paState` to be stopped. Callers add signers and send.
 */
export function closeEscrow(
  forwarder: Program<SplTokenForwarder>,
  authority: PublicKey,
  paState: PublicKey,
  accounts: { mint: PublicKey; escrowAta: PublicKey; recipientAta: PublicKey },
) {
  return forwarder.methods.closeEscrow().accounts({
    authority,
    escrowAta: accounts.escrowAta,
    recipientAta: accounts.recipientAta,
    tokenMint: accounts.mint,
    paState,
  });
}

/** `set_emergency_caller` by the committee; only while the adapter at `paState` is stopped. */
export function setEmergencyCaller(
  forwarder: Program<SplTokenForwarder>,
  committee: PublicKey,
  paState: PublicKey,
  caller: PublicKey,
) {
  return forwarder.methods.setEmergencyCaller(caller).accounts({ committee, paState });
}

/** `close_config` by the committee `authority`; requires the adapter at `paState` to be stopped. */
export function closeConfig(forwarder: Program<SplTokenForwarder>, authority: PublicKey, paState: PublicKey) {
  return forwarder.methods.closeConfig().accounts({ authority, paState });
}

/** `close_nonce_bitmaps_batch` by the committee `authority` over `bitmaps`; requires the adapter at `paState` to be stopped. */
export function closeNonceBitmapsBatch(
  forwarder: Program<SplTokenForwarder>,
  authority: PublicKey,
  paState: PublicKey,
  bitmaps: PublicKey[],
) {
  return forwarder.methods
    .closeNonceBitmapsBatch()
    .accounts({ authority, paState })
    .remainingAccounts(bitmaps.map((pubkey) => ({ pubkey, isWritable: true, isSigner: false })));
}

/**
 * Close every nonce bitmap the forwarder owns, in batches, as the committee
 * `authority` (signing with `signers`, or the provider wallet when empty);
 * requires the adapter at `paState` to be stopped.
 * Returns how many were closed.
 */
export async function closeAllNonceBitmaps(
  forwarder: Program<SplTokenForwarder>,
  authority: PublicKey,
  paState: PublicKey,
  signers: Keypair[],
): Promise<number> {
  const bitmaps = await forwarder.account.nonceBitmap.all();
  const BATCH_SIZE = 20;
  for (const batch of chunks(bitmaps, BATCH_SIZE)) {
    await closeNonceBitmapsBatch(
      forwarder,
      authority,
      paState,
      batch.map(({ publicKey }) => publicKey),
    )
      .signers(signers)
      .rpc();
  }
  return bitmaps.length;
}

/** `items` split, in order, into runs of at most `size`. */
export function chunks<T>(items: readonly T[], size: number): T[][] {
  const runs: T[][] = [];
  for (let i = 0; i < items.length; i += size) runs.push(items.slice(i, i + size));
  return runs;
}
