/**
 * Instruction builders for the protocol adapter and the SPL token forwarder:
 * the one form of each instruction the operator scripts and the test suite
 * both send. Builders return an Anchor method builder; callers add signers
 * and send.
 */
import { Program } from "@anchor-lang/core";
import { AccountMeta, Keypair, PublicKey, SystemProgram } from "@solana/web3.js";
import { getAssociatedTokenAddressSync, TOKEN_PROGRAM_ID } from "@solana/spl-token";
import { ProtocolAdapter } from "../target/types/protocol_adapter";
import { SplTokenForwarder } from "../target/types/spl_token_forwarder";
import { deriveConfigPda, deriveEscrowPda, derivePaStatePda, deriveProgramDataPda } from "./pda";

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
    program: program.programId,
    programData: deriveProgramDataPda(program.programId),
  });
}

/** `emergency_stop` by `authority`. */
export function emergencyStop(program: Program<ProtocolAdapter>, authority: PublicKey) {
  return program.methods
    .emergencyStop()
    .accountsPartial({ paState: derivePaStatePda(program.programId)[0], authority });
}

/** `set_kind_table_commitment` by `authority`. */
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
): { escrowPda: PublicKey; escrowAta: PublicKey } {
  const escrowPda = deriveEscrowPda(forwarderProgramId);
  return { escrowPda, escrowAta: getAssociatedTokenAddressSync(mint, escrowPda, true) };
}

/**
 * The accounts of an escrow release, in the order the program reads them:
 * the unwrap's remaining accounts after the segment head, and the whole of
 * forward_emergency_call's.
 */
export function escrowTransferAccounts(
  escrowAta: PublicKey,
  recipientAta: PublicKey,
  escrowPda: PublicKey,
): AccountMeta[] {
  return [
    { pubkey: escrowAta, isSigner: false, isWritable: true },
    { pubkey: recipientAta, isSigner: false, isWritable: true },
    { pubkey: escrowPda, isSigner: false, isWritable: false },
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
) {
  return forwarder.methods.initialize(adapterProgramId, logicRef, committee).accounts({ authority });
}

/**
 * Rotate the forwarder config's logic ref in place. `authority` must be the
 * program's upgrade authority; `programData` is the forwarder's own
 * ProgramData unless a test substitutes another program's.
 */
export function setLogicRef(
  forwarder: Program<SplTokenForwarder>,
  authority: PublicKey,
  logicRef: number[],
  programData: PublicKey = deriveProgramDataPda(forwarder.programId),
) {
  return forwarder.methods.setLogicRef(logicRef).accounts({ authority, programData });
}

/** `forward_emergency_call` by `caller`; callers add signers and send. */
export function emergencyWithdraw(
  forwarder: Program<SplTokenForwarder>,
  paState: PublicKey,
  caller: PublicKey,
  withdrawal: { mint: PublicKey; amount: bigint; recipient: PublicKey },
  accounts: { escrowAta: PublicKey; recipientAta: PublicKey; escrowPda: PublicKey },
) {
  return forwarder.methods
    .forwardEmergencyCall(encodeUnwrapInput(withdrawal.mint, withdrawal.amount, withdrawal.recipient))
    .accounts({ caller, paState })
    .remainingAccounts(escrowTransferAccounts(accounts.escrowAta, accounts.recipientAta, accounts.escrowPda));
}

/**
 * `close_escrow` by `authority`, draining the escrow to `recipientAta`;
 * requires the adapter at `paState` to be stopped. Callers add signers and send.
 */
export function closeEscrow(
  forwarder: Program<SplTokenForwarder>,
  authority: PublicKey,
  paState: PublicKey,
  accounts: { mint: PublicKey; escrowPda: PublicKey; escrowAta: PublicKey; recipientAta: PublicKey },
) {
  return forwarder.methods.closeEscrow().accountsPartial({
    authority,
    config: deriveConfigPda(forwarder.programId)[0],
    escrowAta: accounts.escrowAta,
    escrowPda: accounts.escrowPda,
    recipientAta: accounts.recipientAta,
    tokenMint: accounts.mint,
    tokenProgram: TOKEN_PROGRAM_ID,
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
  return forwarder.methods
    .closeConfig()
    .accountsPartial({ authority, config: deriveConfigPda(forwarder.programId)[0], paState });
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
  const config = deriveConfigPda(forwarder.programId)[0];
  const BATCH_SIZE = 20;
  for (let i = 0; i < bitmaps.length; i += BATCH_SIZE) {
    await forwarder.methods
      .closeNonceBitmapsBatch()
      .accountsPartial({ authority, config, paState })
      .remainingAccounts(
        bitmaps
          .slice(i, i + BATCH_SIZE)
          .map(({ publicKey }) => ({ pubkey: publicKey, isWritable: true, isSigner: false })),
      )
      .signers(signers)
      .rpc();
  }
  return bitmaps.length;
}
