import * as anchor from "@coral-xyz/anchor";
import {
  Keypair,
  PublicKey,
  LAMPORTS_PER_SOL,
  SystemProgram,
  Transaction,
} from "@solana/web3.js";
import { createHash } from "crypto";
import { OP_WRAP, OP_UNWRAP } from "./constants";

export async function fundKeypair(
  provider: anchor.AnchorProvider,
  kp: Keypair,
  sol: number
) {
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
 * Drain funded keypairs back to the provider wallet.
 * Call this in an after() hook to recover SOL on devnet.
 */
export async function drainKeypairs(
  provider: anchor.AnchorProvider,
  keypairs: Keypair[],
  label: string
) {
  const MIN_DRAIN = 5000;
  let recovered = 0;
  let drained = 0;
  for (const kp of keypairs) {
    try {
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
    } catch {
      // Best-effort — tx may fail if keypair was already drained
    }
  }
  console.log(
    `  [sol] ${label}: recovered ${(recovered / LAMPORTS_PER_SOL).toFixed(4)} SOL ` +
      `(${drained} keypairs)`
  );
}

/**
 * Create the wrap message hash for Ed25519 signature verification.
 * Must match Rust WrapMessage::to_bytes().
 *
 * Layout (120 bytes):
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
 * Encode wrap input for the SPL Token Forwarder.
 * Layout: op(1) + token_mint(32) + amount(8) + user(32) + nonce(8) +
 *         deadline(8) + action_tree_root(32) + signature(64) + ed25519_ix_index(1) = 186 bytes
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
  let offset = 0;

  input.writeUInt8(OP_WRAP, offset);
  offset += 1;

  tokenMint.toBuffer().copy(input, offset);
  offset += 32;

  input.writeBigUInt64LE(amount, offset);
  offset += 8;

  user.toBuffer().copy(input, offset);
  offset += 32;

  input.writeBigUInt64LE(nonce, offset);
  offset += 8;

  input.writeBigInt64LE(deadline, offset);
  offset += 8;

  actionTreeRoot.copy(input, offset);
  offset += 32;

  signature.copy(input, offset);
  offset += 64;

  input.writeUInt8(ed25519IxIndex, offset);

  return input;
}

/**
 * Encode unwrap input for the SPL Token Forwarder.
 * Layout: op(1) + token_mint(32) + amount(8) + recipient(32) = 73 bytes
 */
export function encodeUnwrapInput(
  tokenMint: PublicKey,
  amount: bigint,
  recipient: PublicKey
): Buffer {
  const input = Buffer.alloc(73);
  let offset = 0;

  input.writeUInt8(OP_UNWRAP, offset);
  offset += 1;

  tokenMint.toBuffer().copy(input, offset);
  offset += 32;

  input.writeBigUInt64LE(amount, offset);
  offset += 8;

  recipient.toBuffer().copy(input, offset);

  return input;
}
