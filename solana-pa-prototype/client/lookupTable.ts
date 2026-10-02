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
  sendAndConfirmTransaction,
} from "@solana/web3.js";
import { TOKEN_PROGRAM_ID } from "@solana/spl-token";
import { getRouterPda, getVerifierEntryPda } from "./verifier";
import { escrowAccounts } from "./instructions";
import { deriveConfigPda, deriveEscrowAuthority, deriveEventAuthorityPda, derivePaStatePda } from "./pda";

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
 * programs (the adapter, compute budget, ed25519) are left out because the
 * message compilers keep them static regardless.
 */
export function settlementLookupKeys(s: SettlementKeySources): PublicKey[] {
  const [paState] = derivePaStatePda(s.paProgram);
  const [eventAuthority] = deriveEventAuthorityPda(s.paProgram);
  const [router] = getRouterPda(s.verifierRouter);
  const [verifierEntry] = getVerifierEntryPda(s.proofSelector, s.verifierRouter);
  const [forwarderConfig] = deriveConfigPda(s.splTokenForwarder);
  const [forwarderEventAuthority] = deriveEventAuthorityPda(s.splTokenForwarder);
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
    forwarderEventAuthority,
    deriveEscrowAuthority(s.splTokenForwarder),
    TOKEN_PROGRAM_ID,
    ...s.mints.map((mint) => escrowAccounts(s.splTokenForwarder, mint).escrowAta),
  ];
}

/** The lookup table at `address`; a missing table is an error. */
export async function fetchLookupTable(connection: Connection, address: PublicKey): Promise<AddressLookupTableAccount> {
  const { value } = await connection.getAddressLookupTable(address);
  if (!value) throw new Error(`lookup table ${address.toBase58()} does not exist`);
  return value;
}

/**
 * Create a table holding `keys`, or extend `existing` with the keys it lacks;
 * `payer` pays and is the authority. Returns the usable table, so this waits
 * for the transaction to be finalized: until the slot that extended the
 * table is finalized, a v0 transaction naming the new keys passes simulation
 * but does not land. `signature` is set only when a transaction was sent.
 */
export async function ensureSettlementLookupTable(
  connection: Connection,
  payer: Keypair,
  keys: PublicKey[],
  existing?: PublicKey,
): Promise<{ table: AddressLookupTableAccount; added: PublicKey[]; signature?: string }> {
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
  let signature: string | undefined;
  if (instructions.length > 0) {
    signature = await sendAndConfirmTransaction(connection, new Transaction().add(...instructions), [payer], {
      preflightCommitment: "confirmed",
      commitment: "finalized",
    });
  }
  return { table: await fetchLookupTable(connection, address), added, signature };
}
