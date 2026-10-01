/**
 * The only builders in this repository for instructions that change an
 * authority or close protocol accounts for good: the loader's SetAuthority on
 * a program's ProgramData (move or renounce the upgrade authority, which is
 * the adapter's and the forwarder's owner), the forwarder's
 * `set_emergency_caller`, `close_escrow`, `close_config` and
 * `close_nonce_bitmaps_batch` (wrap replay protection), and the adapter's
 * dev-build `close_markers_batch` (settlement replay protection). They exist
 * for the tests and refuse any RPC endpoint that is not this machine's, so no
 * repository code can do any of this on devnet or mainnet; there, it is done
 * by hand (docs/OPERATIONS.md).
 */
import { Program } from "@anchor-lang/core";
import { Connection, Keypair, PublicKey, TransactionInstruction } from "@solana/web3.js";
import { ProtocolAdapter } from "../../target/types/protocol_adapter";
import { SplTokenForwarder } from "../../target/types/spl_token_forwarder";
import { BPF_LOADER_UPGRADEABLE, derivePaStatePda, deriveProgramDataPda } from "../../client/pda";

const LOOPBACK_HOSTS = new Set(["127.0.0.1", "localhost", "[::1]"]);
const BATCH_SIZE = 20;

/** `items` split, in order, into runs of at most `size`. */
function chunks<T>(items: readonly T[], size: number): T[][] {
  const runs: T[][] = [];
  for (let i = 0; i < items.length; i += size) runs.push(items.slice(i, i + size));
  return runs;
}

/** Throw unless `connection` talks to a validator on this machine. */
export function assertLocalValidator(connection: Connection): void {
  const host = new URL(connection.rpcEndpoint).hostname;
  if (!LOOPBACK_HOSTS.has(host)) {
    throw new Error(
      `authority and closing instructions are built only against a local validator, not ${host}: ` +
        "on devnet and mainnet they are made by hand",
    );
  }
}

/**
 * The loader's SetAuthority (instruction 4) on `programId`'s ProgramData,
 * signed by `current`, for sending through `connection`: `next` becomes the
 * upgrade authority, or none when `next` is null (the program is final).
 */
export function localSetUpgradeAuthority(
  connection: Connection,
  programId: PublicKey,
  current: PublicKey,
  next: PublicKey | null,
): TransactionInstruction {
  assertLocalValidator(connection);
  return new TransactionInstruction({
    programId: BPF_LOADER_UPGRADEABLE,
    keys: [
      { pubkey: deriveProgramDataPda(programId), isSigner: false, isWritable: true },
      { pubkey: current, isSigner: true, isWritable: false },
      ...(next ? [{ pubkey: next, isSigner: false, isWritable: false }] : []),
    ],
    data: Buffer.from([4, 0, 0, 0]),
  });
}

/** `set_emergency_caller` by the committee; only while the adapter at `paState` is paused. */
export function localSetEmergencyCaller(
  forwarder: Program<SplTokenForwarder>,
  committee: PublicKey,
  paState: PublicKey,
  caller: PublicKey,
) {
  assertLocalValidator(forwarder.provider.connection);
  return forwarder.methods.setEmergencyCaller(caller).accounts({ committee, paState });
}

/**
 * `close_escrow` by `authority`, draining the escrow to `recipientAta`;
 * requires the adapter at `paState` to be paused. Callers add signers and send.
 */
export function localCloseEscrow(
  forwarder: Program<SplTokenForwarder>,
  authority: PublicKey,
  paState: PublicKey,
  accounts: { mint: PublicKey; escrowAta: PublicKey; recipientAta: PublicKey },
) {
  assertLocalValidator(forwarder.provider.connection);
  return forwarder.methods.closeEscrow().accounts({
    authority,
    escrowAta: accounts.escrowAta,
    recipientAta: accounts.recipientAta,
    tokenMint: accounts.mint,
    paState,
  });
}

/** `close_config` by the committee `authority`; requires the adapter at `paState` to be paused. */
export function localCloseConfig(forwarder: Program<SplTokenForwarder>, authority: PublicKey, paState: PublicKey) {
  assertLocalValidator(forwarder.provider.connection);
  return forwarder.methods.closeConfig().accounts({ authority, paState });
}

/** `close_nonce_bitmaps_batch` by the committee `authority` over `bitmaps`; requires the adapter at `paState` to be paused. */
export function localCloseNonceBitmapsBatch(
  forwarder: Program<SplTokenForwarder>,
  authority: PublicKey,
  paState: PublicKey,
  bitmaps: PublicKey[],
) {
  assertLocalValidator(forwarder.provider.connection);
  return forwarder.methods
    .closeNonceBitmapsBatch()
    .accounts({ authority, paState })
    .remainingAccounts(bitmaps.map((pubkey) => ({ pubkey, isWritable: true, isSigner: false })));
}

/**
 * Close every nonce bitmap the forwarder owns, in batches, as the committee
 * `authority` (signing with `signers`, or the provider wallet when empty);
 * requires the adapter at `paState` to be paused. Returns how many were closed.
 */
export async function localCloseAllNonceBitmaps(
  forwarder: Program<SplTokenForwarder>,
  authority: PublicKey,
  paState: PublicKey,
  signers: Keypair[],
): Promise<number> {
  assertLocalValidator(forwarder.provider.connection);
  const bitmaps = await forwarder.account.nonceBitmap.all();
  for (const batch of chunks(bitmaps, BATCH_SIZE)) {
    await localCloseNonceBitmapsBatch(
      forwarder,
      authority,
      paState,
      batch.map(({ publicKey }) => publicKey),
    )
      .signers(signers)
      .rpc();
  }
  return bitmaps.length;
}

/**
 * `close_markers_batch` by `authority` over `markers`; only on a paused
 * adapter built with the `dev-teardown` feature. The method is looked up
 * untyped: naming it in a type would make this module fail to compile
 * against production types, preempting the actionable error thrown when the
 * program was built without the feature.
 */
export function localCloseMarkersBatch(program: Program<ProtocolAdapter>, authority: PublicKey, markers: PublicKey[]) {
  assertLocalValidator(program.provider.connection);
  const method = (program.methods as Record<string, unknown>).closeMarkersBatch;
  if (typeof method !== "function") {
    throw new Error(
      "Instruction 'close_markers_batch' is not present in the program's IDL (target/idl/protocol_adapter.json): " +
        "the program was built without the 'dev-teardown' feature.",
    );
  }
  return (method as () => ReturnType<Program<ProtocolAdapter>["methods"][keyof Program<ProtocolAdapter>["methods"]]>)()
    .accountsPartial({ paState: derivePaStatePda(program.programId)[0], authority })
    .remainingAccounts(markers.map((pubkey) => ({ pubkey, isWritable: true, isSigner: false })));
}

/**
 * Close every marker account (the zero-byte nullifier and root markers) the
 * adapter owns, in batches, as `authority`: the provider wallet, which must
 * be the program's upgrade authority, on a paused adapter. Returns how many
 * were closed.
 */
export async function localCloseAllMarkers(program: Program<ProtocolAdapter>, authority: PublicKey): Promise<number> {
  assertLocalValidator(program.provider.connection);
  const markers = await program.provider.connection.getProgramAccounts(program.programId, {
    filters: [{ dataSize: 0 }],
  });
  for (const batch of chunks(markers, BATCH_SIZE)) {
    await localCloseMarkersBatch(
      program,
      authority,
      batch.map(({ pubkey }) => pubkey),
    ).rpc();
  }
  return markers.length;
}
