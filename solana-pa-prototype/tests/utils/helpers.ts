import * as anchor from "@coral-xyz/anchor";
import { Program } from "@coral-xyz/anchor";
import {
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
import { createHash } from "crypto";
import { ProtocolAdapter } from "../../target/types/protocol_adapter";
import { SplTokenForwarder } from "../../target/types/spl_token_forwarder";
import { VERIFIER_ROUTER_ID } from "../../scripts/verifier-utils";
import {
  EMPTY_KIND_TABLE_COMMITMENT,
  NONCE_BITMAP_ACCOUNT_SIZE,
  NONCE_BITMAP_DATA_OFFSET,
  NONCES_PER_WORD,
  OP_UNWRAP,
  TX_DATA_SEED,
} from "./constants";
import { deriveEscrowPda, derivePaStatePda, deriveProgramDataPda } from "./pda";

export type AccountMeta = { pubkey: PublicKey; isWritable: boolean; isSigner: boolean };

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
    drainTx.recentBlockhash = (await provider.connection.getLatestBlockhash()).blockhash;
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

/**
 * The one set of initialize arguments the whole suite deploys with: the
 * verifier router, the fixture's selector, and the kind-table commitment
 * every fixture's aggregation instance carries. Callers add `.signers()`
 * when the payer is not the provider wallet.
 */
export function paInitializeBuilder(
  program: Program<ProtocolAdapter>,
  payer: PublicKey,
  selector: Buffer
) {
  const [paState] = derivePaStatePda(program.programId);
  return program.methods
    .initialize(
      VERIFIER_ROUTER_ID,
      Array.from(selector),
      Array.from(EMPTY_KIND_TABLE_COMMITMENT)
    )
    .accountsPartial({
      paState,
      payer,
      systemProgram: SystemProgram.programId,
      program: program.programId,
      programData: deriveProgramDataPda(program.programId),
    });
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
 * The unwrap operand (72 bytes: token_mint, amount u64 LE, recipient),
 * which is also the whole input of forward_emergency_call. Prefixed with
 * the op code for forward_call.
 */
export function encodeUnwrapInput(
  tokenMint: PublicKey,
  amount: bigint,
  recipient: PublicKey,
  withOpCode = true
): Buffer {
  const operand = Buffer.alloc(72);
  tokenMint.toBuffer().copy(operand, 0);
  operand.writeBigUInt64LE(amount, 32);
  recipient.toBuffer().copy(operand, 40);
  return withOpCode ? Buffer.concat([Buffer.from([OP_UNWRAP]), operand]) : operand;
}

/** Whether `nonce`'s bit is set in a nonce bitmap account's data. */
export function isNonceUsed(bitmapAccountData: Buffer, nonce: bigint): boolean {
  const bit = Number(nonce % NONCES_PER_WORD);
  const byte = bitmapAccountData[NONCE_BITMAP_DATA_OFFSET + (bit >> 3)];
  return (byte & (1 << (bit & 7))) !== 0;
}

/** A mint's escrow: the forwarder's PDA authority and its associated token account. */
export function escrowAccounts(
  forwarderProgramId: PublicKey,
  mint: PublicKey
): { escrowPda: PublicKey; escrowAta: PublicKey } {
  const [escrowPda] = deriveEscrowPda(forwarderProgramId, mint);
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

/** The remaining accounts of forward_emergency_call, in the order the program reads them. */
export function emergencyWithdrawAccounts(
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
  const bitmaps = await forwarder.provider.connection.getProgramAccounts(forwarder.programId, {
    filters: [{ dataSize: NONCE_BITMAP_ACCOUNT_SIZE }],
  });
  const BATCH_SIZE = 20;
  for (let i = 0; i < bitmaps.length; i += BATCH_SIZE) {
    await forwarder.methods
      .closeNonceBitmapsBatch()
      .accountsPartial({ authority, config: configPda })
      .remainingAccounts(
        bitmaps.slice(i, i + BATCH_SIZE).map(({ pubkey }) => ({ pubkey, isWritable: true, isSigner: false }))
      )
      .signers(signers)
      .rpc();
  }
  return bitmaps.length;
}
