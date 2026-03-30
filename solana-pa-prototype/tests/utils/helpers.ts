import * as anchor from "@coral-xyz/anchor";
import {
  Keypair,
  LAMPORTS_PER_SOL,
  SystemProgram,
  Transaction,
} from "@solana/web3.js";

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
 * Drain SOL from funded keypairs back to the provider wallet.
 */
export async function drainKeypairs(
  provider: anchor.AnchorProvider,
  keypairs: Keypair[]
): Promise<void> {
  for (const kp of keypairs) {
    const balance = await provider.connection.getBalance(kp.publicKey);
    if (balance <= 5000) continue; // not worth the tx fee
    try {
      const tx = new Transaction().add(
        SystemProgram.transfer({
          fromPubkey: kp.publicKey,
          toPubkey: provider.wallet.publicKey,
          lamports: balance - 5000, // leave enough for rent
        })
      );
      tx.feePayer = kp.publicKey;
      await provider.sendAndConfirm(tx, [kp]);
    } catch {
      // Best effort — don't fail test cleanup
    }
  }
}
