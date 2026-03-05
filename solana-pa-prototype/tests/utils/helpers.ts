import * as anchor from "@coral-xyz/anchor";
import {
  Keypair,
  LAMPORTS_PER_SOL,
  SystemProgram,
  Transaction,
} from "@solana/web3.js";
import { createHash } from "crypto";

// Keypairs funded during tests, drained back to the provider wallet in
// afterEach() so devnet SOL circulates across the test run.
export const fundedKeypairs: Keypair[] = [];

export async function airdrop(
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
  fundedKeypairs.push(kp);
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
  const OP_WRAP = 0;
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
  const OP_UNWRAP = 1;
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
