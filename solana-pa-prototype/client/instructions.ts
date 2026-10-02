/**
 * Instruction builders for the protocol adapter and the SPL token forwarder:
 * the one form of each instruction the operator scripts and the test suite
 * both send. Builders return an Anchor method builder; callers add signers
 * and send.
 */
import { BN, Program } from "@anchor-lang/core";
import { AccountMeta, PublicKey, SystemProgram, SYSVAR_INSTRUCTIONS_PUBKEY } from "@solana/web3.js";
import { getAssociatedTokenAddressSync, TOKEN_PROGRAM_ID } from "@solana/spl-token";
import { ProtocolAdapter } from "../target/types/protocol_adapter";
import { getVerifierEntryPda } from "./verifier";
import { SplTokenForwarder } from "../target/types/spl_token_forwarder";
import { deriveConfigPda, deriveEscrowAuthority, derivePaStatePda, deriveProgramDataPda } from "./pda";

// Protocol adapter governance

/**
 * The adapter's `initialize`, paid by `payer`, pinning the verifier; it
 * starts on the empty kind table and refuses a verifier the router has paused,
 * read from the router's verifier entry for `proofSelector`.
 */
export function initializeAdapter(
  program: Program<ProtocolAdapter>,
  payer: PublicKey,
  verifierRouter: PublicKey,
  proofSelector: number[],
) {
  return program.methods.initialize(verifierRouter, proofSelector).accountsPartial({
    paState: derivePaStatePda(program.programId)[0],
    verifierEntry: getVerifierEntryPda(Buffer.from(proofSelector), verifierRouter)[0],
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

/** `pause` by the upgrade authority: settlement stops until `unpause`. */
export function pauseAdapter(program: Program<ProtocolAdapter>, authority: PublicKey) {
  return program.methods.pause().accountsPartial({ paState: derivePaStatePda(program.programId)[0], authority });
}

/** `unpause` by the upgrade authority: settlement resumes. */
export function unpauseAdapter(program: Program<ProtocolAdapter>, authority: PublicKey) {
  return program.methods.unpause().accountsPartial({ paState: derivePaStatePda(program.programId)[0], authority });
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
