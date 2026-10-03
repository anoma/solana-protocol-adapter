/**
 * The programs' events are Anchor CPI events: inner instructions of the
 * emitting program whose data is Anchor's event tag followed by the event's
 * discriminator and Borsh body.
 */
import * as anchor from "@anchor-lang/core";
import { Connection } from "@solana/web3.js";

/** Anchor's CPI event tag: the fixed 8-byte `EVENT_IX_TAG_LE`, the little-endian encoding of the u64 0x1d9acb512ea545e4. */
const EVENT_IX_TAG_LE = Buffer.from([0xe4, 0x45, 0xa5, 0x2e, 0x51, 0xcb, 0x9a, 0x1d]);

/** The CPI events `emitter` emitted in the confirmed transaction `tx`, in emission order. */
export function parseCpiEvents(tx: anchor.web3.VersionedTransactionResponse, emitter: anchor.Program<any>) {
  const keys = tx.transaction.message.getAccountKeys({
    accountKeysFromLookups: tx.meta?.loadedAddresses,
  });
  const coder = new anchor.BorshCoder(emitter.idl);
  const events: { name: string; data: any }[] = [];
  for (const group of tx.meta?.innerInstructions ?? []) {
    for (const ix of group.instructions) {
      if (!keys.get(ix.programIdIndex)?.equals(emitter.programId)) continue;
      const data = Buffer.from(anchor.utils.bytes.bs58.decode(ix.data));
      if (data.length < 16 || !data.subarray(0, 8).equals(EVENT_IX_TAG_LE)) continue;
      const decoded = coder.events.decode(data.subarray(8).toString("base64"));
      if (decoded) events.push(decoded);
    }
  }
  return events;
}

/** The CPI events `emitter` emitted in the confirmed transaction `signature`. */
export async function cpiEventsOfSignature(connection: Connection, emitter: anchor.Program<any>, signature: string) {
  const tx = await connection.getTransaction(signature, { commitment: "confirmed", maxSupportedTransactionVersion: 0 });
  if (!tx) throw new Error(`transaction ${signature} is not fetchable at confirmed commitment`);
  return parseCpiEvents(tx, emitter);
}
