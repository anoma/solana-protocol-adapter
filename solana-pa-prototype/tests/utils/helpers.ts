import * as anchor from "@coral-xyz/anchor";
import { Program } from "@coral-xyz/anchor";
import {
  Keypair,
  LAMPORTS_PER_SOL,
  PublicKey,
  SystemProgram,
  Transaction,
} from "@solana/web3.js";
import { createHash } from "crypto";
import { ProtocolAdapter } from "../../target/types/protocol_adapter";
import { VERIFIER_ROUTER_ID } from "../../scripts/verifier-utils";
import {
  EMPTY_KIND_TABLE_COMMITMENT,
  NONCE_BITMAP_DATA_OFFSET,
  NONCES_PER_WORD,
  OP_UNWRAP,
  OP_WRAP,
  TX_DATA_SEED,
} from "./constants";
import { derivePaStatePda, deriveProgramDataPda } from "./pda";

/**
 * Fund a keypair from the provider wallet, topping up to the requested amount.
 * Returns the keypair for tracking (caller can add to a drain list).
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

// SPL token forwarder encodings (programs/spl-token-forwarder/src/state.rs)

/**
 * sha256 of the 120-byte wrap message the user authorizes; must match
 * Rust `WrapMessage::to_bytes()` + `hash()`.
 *
 * | Offset | Size | Field            |
 * |--------|------|------------------|
 * | 0      | 32   | forwarder_id     |
 * | 32     | 32   | token_mint       |
 * | 64     | 8    | amount (u64 LE)  |
 * | 72     | 8    | nonce (u64 LE)   |
 * | 80     | 8    | deadline (i64 LE)|
 * | 88     | 32   | action_tree_root |
 */
export function createWrapMessageHash(
  forwarderId: PublicKey,
  tokenMint: PublicKey,
  amount: bigint,
  nonce: bigint,
  deadline: bigint,
  actionTreeRoot: Buffer
): Buffer {
  const message = Buffer.alloc(120);
  forwarderId.toBuffer().copy(message, 0);
  tokenMint.toBuffer().copy(message, 32);
  message.writeBigUInt64LE(amount, 64);
  message.writeBigUInt64LE(nonce, 72);
  message.writeBigInt64LE(deadline, 80);
  actionTreeRoot.copy(message, 88);
  return createHash("sha256").update(message).digest();
}

/**
 * forward_call input for a wrap: op(1) + token_mint(32) + amount(8) +
 * user(32) + nonce(8) + deadline(8) + action_tree_root(32) + signature(64)
 * + ed25519_ix_index(1) = 186 bytes.
 */
export function encodeWrapInput(
  tokenMint: PublicKey,
  amount: bigint,
  user: PublicKey,
  nonce: bigint,
  deadline: bigint,
  actionTreeRoot: Buffer,
  signature: Buffer,
  ed25519IxIndex: number
): Buffer {
  const input = Buffer.alloc(186);
  input.writeUInt8(OP_WRAP, 0);
  tokenMint.toBuffer().copy(input, 1);
  input.writeBigUInt64LE(amount, 33);
  user.toBuffer().copy(input, 41);
  input.writeBigUInt64LE(nonce, 73);
  input.writeBigInt64LE(deadline, 81);
  actionTreeRoot.copy(input, 89);
  signature.copy(input, 121);
  input.writeUInt8(ed25519IxIndex, 185);
  return input;
}

/** forward_call input for an unwrap: op(1) + token_mint(32) + amount(8) + recipient(32) = 73 bytes. */
export function encodeUnwrapInput(
  tokenMint: PublicKey,
  amount: bigint,
  recipient: PublicKey
): Buffer {
  const input = Buffer.alloc(73);
  input.writeUInt8(OP_UNWRAP, 0);
  tokenMint.toBuffer().copy(input, 1);
  input.writeBigUInt64LE(amount, 33);
  recipient.toBuffer().copy(input, 41);
  return input;
}

/** forward_emergency_call input: op(1) + token_mint(32) + amount(8) + recipient(32) = 73 bytes. */
export function encodeEmergencyWithdrawInput(
  op: number,
  tokenMint: PublicKey,
  amount: bigint,
  recipient: PublicKey
): Buffer {
  const input = encodeUnwrapInput(tokenMint, amount, recipient);
  input.writeUInt8(op, 0);
  return input;
}

/** Whether `nonce`'s bit is set in a nonce bitmap account's data. */
export function isNonceUsed(bitmapAccountData: Buffer, nonce: bigint): boolean {
  const bit = Number(nonce % NONCES_PER_WORD);
  const byte = bitmapAccountData[NONCE_BITMAP_DATA_OFFSET + (bit >> 3)];
  return (byte & (1 << (bit & 7))) !== 0;
}
