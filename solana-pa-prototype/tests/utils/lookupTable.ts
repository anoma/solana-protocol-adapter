import * as anchor from "@coral-xyz/anchor";
import {
  AddressLookupTableAccount,
  AddressLookupTableProgram,
  Connection,
  Keypair,
  PublicKey,
  SystemProgram,
  SYSVAR_CLOCK_PUBKEY,
  SYSVAR_INSTRUCTIONS_PUBKEY,
  Transaction,
  TransactionInstruction,
  TransactionMessage,
  VersionedTransaction,
  sendAndConfirmTransaction,
} from "@solana/web3.js";
import { TOKEN_PROGRAM_ID } from "@solana/spl-token";
import { getRouterPda, getVerifierEntryPda } from "../../scripts/verifier-utils";
import { escrowAccounts } from "./helpers";
import { deriveConfigPda, deriveEventAuthorityPda, derivePaStatePda } from "./pda";

/** What fixes a deployment's settlement key set. */
export interface SettlementKeySources {
  paProgram: PublicKey;
  verifierRouter: PublicKey;
  proofSelector: Buffer | Uint8Array;
  verifierProgram: PublicKey;
  blockTimeForwarder: PublicKey;
  splTokenForwarder: PublicKey;
  mints: PublicKey[];
}

/**
 * The accounts every settlement of a deployment carries that are neither a
 * signer nor an invoked program: the ones a lookup table can hold. Invoked
 * programs (the adapter, compute budget, ed25519) are not listed because
 * both message compilers keep them static whether or not a table has them.
 * A table entry need not exist on chain, so forwarder and escrow keys are
 * listed before those accounts are created.
 */
export function settlementLookupKeys(s: SettlementKeySources): PublicKey[] {
  const [paState] = derivePaStatePda(s.paProgram);
  const [eventAuthority] = deriveEventAuthorityPda(s.paProgram);
  const [router] = getRouterPda(s.verifierRouter);
  const [verifierEntry] = getVerifierEntryPda(s.proofSelector, s.verifierRouter);
  const [forwarderConfig] = deriveConfigPda(s.splTokenForwarder);
  return [
    paState,
    SystemProgram.programId,
    s.verifierRouter,
    router,
    verifierEntry,
    s.verifierProgram,
    eventAuthority,
    SYSVAR_INSTRUCTIONS_PUBKEY,
    SYSVAR_CLOCK_PUBKEY,
    s.blockTimeForwarder,
    s.splTokenForwarder,
    forwarderConfig,
    TOKEN_PROGRAM_ID,
    ...s.mints.flatMap((mint) => {
      const { escrowPda, escrowAta } = escrowAccounts(s.splTokenForwarder, mint);
      return [escrowPda, escrowAta];
    }),
  ];
}

export async function fetchLookupTable(connection: Connection, address: PublicKey): Promise<AddressLookupTableAccount> {
  const { value } = await connection.getAddressLookupTable(address);
  if (!value) throw new Error(`lookup table ${address.toBase58()} does not exist`);
  return value;
}

/**
 * Create a table holding `keys`, or extend `existing` with the keys it lacks.
 * `payer` pays and is the table's authority. Returns once the table is
 * usable: a table extended in slot N is usable from slot N+1, so this waits
 * for the extending slot to pass. `signature` is set only when a
 * transaction was sent.
 */
export async function ensureSettlementLookupTable(
  connection: Connection,
  payer: Keypair,
  keys: PublicKey[],
  existing?: PublicKey,
): Promise<{ address: PublicKey; added: PublicKey[]; signature?: string }> {
  const instructions: TransactionInstruction[] = [];
  let address = existing;
  let present: PublicKey[] = [];
  if (address) {
    present = (await fetchLookupTable(connection, address)).state.addresses;
  } else {
    const [createIx, created] = AddressLookupTableProgram.createLookupTable({
      authority: payer.publicKey,
      payer: payer.publicKey,
      recentSlot: await connection.getSlot("finalized"),
    });
    address = created;
    instructions.push(createIx);
  }
  const added: PublicKey[] = [];
  for (const key of keys) {
    if (!present.some((p) => p.equals(key)) && !added.some((a) => a.equals(key))) added.push(key);
  }
  if (added.length > 0) {
    instructions.push(
      AddressLookupTableProgram.extendLookupTable({
        lookupTable: address,
        authority: payer.publicKey,
        payer: payer.publicKey,
        addresses: added,
      }),
    );
  }
  if (instructions.length === 0) return { address, added };
  const signature = await sendAndConfirmTransaction(connection, new Transaction().add(...instructions), [payer], {
    commitment: "confirmed",
  });
  const tx = await connection.getTransaction(signature, { commitment: "confirmed", maxSupportedTransactionVersion: 0 });
  if (!tx) throw new Error(`lookup table transaction ${signature} not found after confirmation`);
  while ((await connection.getSlot("confirmed")) <= tx.slot) {
    await new Promise((resolve) => setTimeout(resolve, 400));
  }
  return { address, added, signature };
}

/**
 * Compile `instructions` into a v0 message against `table` with the
 * provider wallet as fee payer, sign with the wallet and `signers`, send
 * and confirm. The provider wraps a failed transaction's logs into the
 * thrown error the same way it does for legacy transactions.
 */
export async function sendV0(
  provider: anchor.AnchorProvider,
  instructions: TransactionInstruction[],
  signers: Keypair[],
  table: AddressLookupTableAccount,
): Promise<string> {
  const { blockhash } = await provider.connection.getLatestBlockhash("confirmed");
  const message = new TransactionMessage({
    payerKey: provider.wallet.publicKey,
    recentBlockhash: blockhash,
    instructions,
  }).compileToV0Message([table]);
  return provider.sendAndConfirm(new VersionedTransaction(message), signers);
}
