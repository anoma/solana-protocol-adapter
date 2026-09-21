import * as anchor from "@coral-xyz/anchor";
import { Program } from "@coral-xyz/anchor";
import {
  AccountMeta,
  Connection,
  Keypair,
  LAMPORTS_PER_SOL,
  PublicKey,
  SystemProgram,
  Transaction,
} from "@solana/web3.js";
import {
  createMint,
  getAssociatedTokenAddressSync,
  getOrCreateAssociatedTokenAccount,
  mintTo,
  TOKEN_PROGRAM_ID,
} from "@solana/spl-token";
import { assert } from "chai";
import { createHash } from "crypto";
import { ProtocolAdapter } from "../../target/types/protocol_adapter";
import { SplTokenForwarder } from "../../target/types/spl_token_forwarder";
import { TX_DATA_SEED } from "./constants";
import { deriveProgramDataPda } from "./pda";

/**
 * Fund a keypair from the provider wallet, topping up to the requested amount.
 */
export async function fundKeypair(
  provider: anchor.AnchorProvider,
  kp: Keypair,
  sol: number
): Promise<void> {
  const needed = sol * LAMPORTS_PER_SOL;
  const balance = await provider.connection.getBalance(kp.publicKey);
  if (balance >= needed) return;

  const tx = new Transaction().add(
    SystemProgram.transfer({
      fromPubkey: provider.wallet.publicKey,
      toPubkey: kp.publicKey,
      lamports: needed - balance,
    })
  );
  await provider.sendAndConfirm(tx);
}

/**
 * Drain funded keypairs back to the provider wallet so the same SOL
 * circulates across the run. Uses sendRawTransaction directly —
 * provider.sendAndConfirm would try to co-sign with the wallet.
 * Returns the lamports recovered and how many keypairs held any.
 */
export async function drainKeypairs(
  provider: anchor.AnchorProvider,
  keypairs: Keypair[]
): Promise<{ recovered: number; drained: number }> {
  const MIN_DRAIN = 5000;
  let recovered = 0;
  let drained = 0;
  const { blockhash } = await provider.connection.getLatestBlockhash();
  for (const kp of keypairs) {
    const balance = await provider.connection.getBalance(kp.publicKey);
    if (balance <= MIN_DRAIN) continue;
    const drainAmount = balance - MIN_DRAIN;
    const drainTx = new Transaction().add(
      SystemProgram.transfer({
        fromPubkey: kp.publicKey,
        toPubkey: provider.wallet.publicKey,
        lamports: drainAmount,
      })
    );
    drainTx.recentBlockhash = blockhash;
    drainTx.feePayer = kp.publicKey;
    drainTx.sign(kp);
    const sig = await provider.connection.sendRawTransaction(drainTx.serialize());
    await provider.connection.confirmTransaction(sig);
    recovered += drainAmount;
    drained++;
  }
  return { recovered, drained };
}

/** A funder that remembers what it funded, so a suite can drain it all in `after`. */
export function makeFunder(provider: anchor.AnchorProvider) {
  const funded: Keypair[] = [];
  return {
    async fund(kp: Keypair, sol: number) {
      await fundKeypair(provider, kp, sol);
      funded.push(kp);
    },
    async drainAll() {
      await drainKeypairs(provider, funded);
      funded.length = 0;
    },
  };
}

/** A keypair every test file can rebuild from the same label. */
export function seededKeypair(label: string): Keypair {
  return Keypair.fromSeed(createHash("sha256").update(label).digest());
}

/** Everything an error carries that names the failure: message, Anchor code, and program logs. */
export function errorHaystack(e: any): string {
  const parts: string[] = [];
  if (e?.message) parts.push(e.message);
  if (e?.error?.errorMessage) parts.push(e.error.errorMessage);
  if (e?.error?.errorCode?.code) parts.push(e.error.errorCode.code);
  const logs: string[] = e?.logs ?? e?.error?.logs ?? [];
  parts.push(...logs);
  return parts.join("\n");
}

/** Assert that `action` rejects with an error naming `expected` somewhere in its message, code or logs. */
export async function assertRejects(action: Promise<unknown>, expected: RegExp | string): Promise<void> {
  try {
    await action;
  } catch (e: any) {
    assert.match(errorHaystack(e), expected instanceof RegExp ? expected : new RegExp(expected));
    return;
  }
  assert.fail(`expected rejection matching ${expected}`);
}

/**
 * Create a TxData account under `authority` and write `payload` into it in
 * 700-byte chunks. The upload id is the wall clock so consecutive uploads
 * by one authority never collide.
 */
export async function uploadTxData(
  program: Program<ProtocolAdapter>,
  paState: PublicKey,
  authority: Keypair,
  payload: Buffer,
  expiresSlotOverride?: anchor.BN
): Promise<{ uploadId: anchor.BN; uploadIdLe: Buffer; txData: PublicKey; expiresSlot: anchor.BN }> {
  const uploadId = new anchor.BN(Date.now());
  const uploadIdLe = Buffer.alloc(8);
  uploadIdLe.writeBigUInt64LE(BigInt(uploadId.toString()));
  const [txData] = PublicKey.findProgramAddressSync(
    [TX_DATA_SEED, authority.publicKey.toBuffer(), uploadIdLe],
    program.programId
  );
  const expiresSlot =
    expiresSlotOverride ??
    new anchor.BN((await program.provider.connection.getSlot("confirmed")) + 10_000);
  await program.methods
    .txdataInit(uploadId, payload.length, expiresSlot)
    .accountsPartial({
      paState,
      txData,
      authority: authority.publicKey,
      systemProgram: SystemProgram.programId,
    })
    .signers([authority])
    .rpc();

  const chunkSize = 700;
  for (let offset = 0; offset < payload.length; offset += chunkSize) {
    const chunk = payload.subarray(offset, Math.min(payload.length, offset + chunkSize));
    await program.methods
      .txdataWrite(uploadId, offset, chunk)
      .accountsPartial({ txData, authority: authority.publicKey })
      .signers([authority])
      .rpc();
  }
  return { uploadId, uploadIdLe, txData, expiresSlot };
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

/** A mint's escrow: the forwarder's PDA authority and its associated token account. */
export function escrowAccounts(
  forwarderProgramId: PublicKey,
  mint: PublicKey
): { escrowPda: PublicKey; escrowAta: PublicKey } {
  const [escrowPda] = PublicKey.findProgramAddressSync([Buffer.from("escrow"), mint.toBuffer()], forwarderProgramId);
  return { escrowPda, escrowAta: getAssociatedTokenAddressSync(mint, escrowPda, true) };
}

/**
 * A fresh 6-decimal mint with `payer` as its authority, its escrow ATA
 * created and holding `amount` raw units.
 */
export async function createFundedEscrow(
  provider: anchor.AnchorProvider,
  forwarderProgramId: PublicKey,
  payer: Keypair,
  amount: bigint
): Promise<{ mint: PublicKey; escrowPda: PublicKey; escrowAta: PublicKey }> {
  const mint = await createMint(provider.connection, payer, payer.publicKey, null, 6);
  const { escrowPda, escrowAta } = escrowAccounts(forwarderProgramId, mint);
  await getOrCreateAssociatedTokenAccount(provider.connection, payer, mint, escrowPda, true);
  await mintTo(provider.connection, payer, mint, escrowAta, payer, Number(amount));
  return { mint, escrowPda, escrowAta };
}

/** The forwarder's `initialize`; callers add signers and send. */
export function initializeForwarder(
  forwarder: Program<SplTokenForwarder>,
  adapterProgramId: PublicKey,
  logicRef: number[],
  committee: PublicKey,
  authority: PublicKey
) {
  return forwarder.methods
    .initialize(adapterProgramId, logicRef, committee)
    .accounts({ authority });
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
  programData: PublicKey = deriveProgramDataPda(forwarder.programId)
) {
  return forwarder.methods.setLogicRef(logicRef).accounts({ authority, programData });
}

/** Any 32 bytes that are not a real logic ref. */
export const randomRef = (): number[] => Array.from(Keypair.generate().publicKey.toBytes());

/**
 * The accounts of an escrow release, in the order the program reads them:
 * the unwrap's remaining accounts after the segment head, and the whole of
 * forward_emergency_call's.
 */
export function escrowTransferAccounts(
  escrowAta: PublicKey,
  recipientAta: PublicKey,
  escrowPda: PublicKey
): AccountMeta[] {
  return [
    { pubkey: escrowAta, isSigner: false, isWritable: true },
    { pubkey: recipientAta, isSigner: false, isWritable: true },
    { pubkey: escrowPda, isSigner: false, isWritable: false },
    { pubkey: TOKEN_PROGRAM_ID, isSigner: false, isWritable: false },
  ];
}

/** `forward_emergency_call` by `caller`; callers add signers and send. */
export function emergencyWithdraw(
  forwarder: Program<SplTokenForwarder>,
  paState: PublicKey,
  caller: PublicKey,
  withdrawal: { mint: PublicKey; amount: bigint; recipient: PublicKey },
  accounts: { escrowAta: PublicKey; recipientAta: PublicKey; escrowPda: PublicKey }
) {
  return forwarder.methods
    .forwardEmergencyCall(encodeUnwrapInput(withdrawal.mint, withdrawal.amount, withdrawal.recipient))
    .accounts({ caller, paState })
    .remainingAccounts(escrowTransferAccounts(accounts.escrowAta, accounts.recipientAta, accounts.escrowPda));
}

/** `close_escrow` by `authority`, draining the escrow to `recipientAta`; callers add signers and send. */
export function closeEscrow(
  forwarder: Program<SplTokenForwarder>,
  configPda: PublicKey,
  authority: PublicKey,
  accounts: { mint: PublicKey; escrowPda: PublicKey; escrowAta: PublicKey; recipientAta: PublicKey }
) {
  return forwarder.methods.closeEscrow().accountsPartial({
    authority,
    config: configPda,
    escrowAta: accounts.escrowAta,
    escrowPda: accounts.escrowPda,
    recipientAta: accounts.recipientAta,
    tokenMint: accounts.mint,
    tokenProgram: TOKEN_PROGRAM_ID,
  });
}

/**
 * Close every nonce bitmap the forwarder owns, in batches, as the committee
 * `authority` (signing with `signers`, or the provider wallet when empty).
 * Returns how many were closed.
 */
export async function closeAllNonceBitmaps(
  forwarder: Program<SplTokenForwarder>,
  configPda: PublicKey,
  authority: PublicKey,
  signers: Keypair[]
): Promise<number> {
  const bitmaps = await forwarder.account.nonceBitmap.all();
  const BATCH_SIZE = 20;
  for (let i = 0; i < bitmaps.length; i += BATCH_SIZE) {
    await forwarder.methods
      .closeNonceBitmapsBatch()
      .accountsPartial({ authority, config: configPda })
      .remainingAccounts(
        bitmaps.slice(i, i + BATCH_SIZE).map(({ publicKey }) => ({ pubkey: publicKey, isWritable: true, isSigner: false }))
      )
      .signers(signers)
      .rpc();
  }
  return bitmaps.length;
}

/** Resolve once the confirmed slot is past `targetSlot`; fail after `timeoutMs`. */
export async function waitForSlotPast(
  connection: Connection,
  targetSlot: number,
  timeoutMs: number = 30000
): Promise<void> {
  const start = Date.now();
  while (Date.now() - start < timeoutMs) {
    const slot = await connection.getSlot("confirmed");
    if (slot > targetSlot) return;
    await new Promise((r) => setTimeout(r, 400));
  }
  throw new Error(`Timed out waiting for slot past ${targetSlot} after ${timeoutMs}ms`);
}
