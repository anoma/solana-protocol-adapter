/**
 * Test-suite helpers: funding and draining keypairs, error assertions,
 * TxData uploads, and v1 settlement transactions.
 */
import { keccak_256 } from "@noble/hashes/sha3";
import * as anchor from "@anchor-lang/core";
import { Idl, Program } from "@anchor-lang/core";
import {
  Connection,
  Keypair,
  LAMPORTS_PER_SOL,
  PublicKey,
  SendTransactionError,
  SystemProgram,
  Transaction,
  TransactionInstruction,
} from "@solana/web3.js";
import {
  AccountRole,
  address,
  appendTransactionMessageInstructions,
  type Blockhash,
  compileTransaction,
  createKeyPairFromBytes,
  createTransactionMessage,
  getBase64EncodedWireTransaction,
  type Instruction,
  pipe,
  setTransactionMessageComputeUnitLimit,
  setTransactionMessageFeePayer,
  setTransactionMessageHeapSize,
  setTransactionMessageLifetimeUsingBlockhash,
  setTransactionMessageLoadedAccountsDataSizeLimit,
  signTransaction,
} from "@solana/kit";
import { assert } from "chai";
import { execFileSync } from "child_process";
import { ProtocolAdapter } from "../../target/types/protocol_adapter";
import { deriveTxDataPda } from "../../client/pda";

/**
 * Fund a keypair from the provider wallet, topping up to the requested amount.
 */
export async function fundKeypair(provider: anchor.AnchorProvider, kp: Keypair, sol: number): Promise<void> {
  const needed = sol * LAMPORTS_PER_SOL;
  const balance = await provider.connection.getBalance(kp.publicKey);
  if (balance >= needed) return;

  const tx = new Transaction().add(
    SystemProgram.transfer({
      fromPubkey: provider.wallet.publicKey,
      toPubkey: kp.publicKey,
      lamports: needed - balance,
    }),
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
  keypairs: Keypair[],
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
      }),
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
  async function fund(kp: Keypair, sol: number) {
    await fundKeypair(provider, kp, sol);
    funded.push(kp);
  }
  return {
    fund,
    /** A new keypair holding `sol`. */
    async fresh(sol: number): Promise<Keypair> {
      const kp = Keypair.generate();
      await fund(kp, sol);
      return kp;
    },
    async drainAll() {
      const drained = await drainKeypairs(provider, funded);
      funded.length = 0;
      return drained;
    },
  };
}

/**
 * How a transaction failed, read from its logs: the program whose frame
 * raised the error and the reason that frame's `Program <id> failed: <reason>`
 * line gives. The raising frame is the innermost failing one: a failed CPI
 * logs its own failure line first, and every caller frame then repeats the
 * same error on its own line. `frameLogs` are the lines the raising frame
 * logged itself, without its callees' lines.
 */
type TransactionFailure = { program: string; reason: string; frameLogs: string[] };

/**
 * An expected failure: the program whose frame raises it, and either `error`,
 * a custom error named as the program's raw IDL names it, or one of Anchor's
 * framework errors (which every Anchor program raises from its own frame),
 * with optionally the `account` an Anchor constraint error names; or `code`,
 * a custom error number, for programs without an IDL here; or `reason`, a
 * pattern for a runtime failure that carries no custom code, whose text
 * embeds per-run data such as addresses.
 */
export type ExpectedFailure =
  | { program: { programId: PublicKey; rawIdl: Idl }; error: string; account?: string }
  | { program: PublicKey; code: number }
  | { program: PublicKey; reason: RegExp };

/** The logs a rejected send or `.rpc()` carries: web3's `SendTransactionError`, or Anchor's translated errors. */
function transactionLogsOf(e: any): string[] {
  const logs = e instanceof SendTransactionError ? e.transactionError.logs : e?.logs;
  assert.isTrue(Array.isArray(logs), `rejection carries no transaction logs:\n${e}`);
  return logs;
}

/** The failure `logs` record; fails if no program frame failed. */
function transactionFailureOf(logs: string[]): TransactionFailure {
  const frames: { program: string; logs: string[] }[] = [];
  for (const line of logs) {
    const invoke = /^Program (\w+) invoke \[\d+\]$/.exec(line);
    if (invoke) {
      frames.push({ program: invoke[1], logs: [] });
      continue;
    }
    if (/^Program \w+ success$/.test(line)) {
      frames.pop();
      continue;
    }
    const failed = /^Program (\w+) failed: (.*)$/.exec(line);
    if (failed) {
      const frame = frames[frames.length - 1];
      assert.equal(frame?.program, failed[1], `failure line outside its program's frame: ${line}`);
      return { program: failed[1], reason: failed[2], frameLogs: frame.logs };
    }
    frames[frames.length - 1]?.logs.push(line);
  }
  assert.fail(`no program frame failed in the logs:\n${logs.join("\n")}`);
}

/** The error number of `name`: an IDL error of `program`, else an Anchor framework error. */
function errorCodeOf(program: { rawIdl: Idl }, name: string): number {
  const code =
    program.rawIdl.errors?.find((e) => e.name === name)?.code ?? (anchor.LangErrorCode as Record<string, number>)[name];
  assert.isDefined(
    code,
    `${name} is neither an error of ${program.rawIdl.metadata.name} nor an Anchor framework error`,
  );
  return code;
}

/** The failure reason `expected` names, as a pattern over the whole reason. */
function expectedReasonOf(expected: ExpectedFailure): RegExp {
  if ("reason" in expected) return expected.reason;
  const code = "error" in expected ? errorCodeOf(expected.program, expected.error) : expected.code;
  return new RegExp(`^custom program error: 0x${code.toString(16)}$`);
}

/** Assert that `action` rejects with the transaction failure `expected`. */
export async function assertFails(action: Promise<unknown>, expected: ExpectedFailure): Promise<void> {
  let error: unknown;
  try {
    await action;
  } catch (e) {
    error = e;
  }
  assert.isDefined(error, "expected the transaction to fail");
  const logs = transactionLogsOf(error);
  const failure = transactionFailureOf(logs);
  const program = "error" in expected ? expected.program.programId : expected.program;
  const reason = expectedReasonOf(expected);
  const context = `\nLogs:\n${logs.join("\n")}`;
  assert.equal(failure.program, program.toBase58(), `failing program${context}`);
  assert.match(failure.reason, reason, `failure reason of ${program.toBase58()}${context}`);
  if ("error" in expected && expected.account !== undefined) {
    const origin = failure.frameLogs
      .map((l) => /^Program log: AnchorError caused by account: (\w+)\./.exec(l)?.[1])
      .find((a) => a !== undefined);
    assert.equal(origin, expected.account, `account the error names${context}`);
  }
}

/** A TxData upload: its id, the id's little-endian seed bytes, its account, and the slot it expires at. */
export type TxDataUpload = { uploadId: anchor.BN; uploadIdLe: Buffer; txData: PublicKey; expiresSlot: anchor.BN };

/**
 * A new TxData upload id under `authority`, its little-endian seed bytes and
 * the upload's account. The id is the wall clock so consecutive uploads by
 * one authority never collide.
 */
export function freshUploadId(paProgramId: PublicKey, authority: PublicKey): Omit<TxDataUpload, "expiresSlot"> {
  const uploadId = new anchor.BN(Date.now());
  const uploadIdLe = Buffer.alloc(8);
  uploadIdLe.writeBigUInt64LE(BigInt(uploadId.toString()));
  return { uploadId, uploadIdLe, txData: deriveTxDataPda(paProgramId, authority, uploadIdLe) };
}

/**
 * Create an empty TxData account of `payloadSize` bytes under `authority`,
 * expiring `expiresSlotOverride`, or else midway between the soonest and the
 * latest expiry the adapter's bounds allow from now.
 */
export async function initTxData(
  program: Program<ProtocolAdapter>,
  paState: PublicKey,
  authority: Keypair,
  payloadSize: number,
  expiresSlotOverride?: anchor.BN,
): Promise<TxDataUpload> {
  const { uploadId, uploadIdLe, txData } = freshUploadId(program.programId, authority.publicKey);
  const expiresSlot = expiresSlotOverride ?? (await midExpirySlot(program, paState));
  await program.methods
    .txdataInit(uploadId, payloadSize, expiresSlot)
    .accountsPartial({
      paState,
      txData,
      authority: authority.publicKey,
      systemProgram: SystemProgram.programId,
    })
    .signers([authority])
    .rpc();
  return { uploadId, uploadIdLe, txData, expiresSlot };
}

/** The slot midway between the soonest and the latest expiry the adapter's bounds allow from now. */
async function midExpirySlot(program: Program<ProtocolAdapter>, paState: PublicKey): Promise<anchor.BN> {
  const { minExpirySlots, maxExpirySlots } = await program.account.paStateAccount.fetch(paState);
  const slot = await program.provider.connection.getSlot("confirmed");
  return new anchor.BN(slot).add(minExpirySlots.add(maxExpirySlots).divn(2));
}

/** Create a TxData account under `authority` and write `payload` into it in 700-byte chunks. */
export async function uploadTxData(
  program: Program<ProtocolAdapter>,
  paState: PublicKey,
  authority: Keypair,
  payload: Buffer,
  expiresSlotOverride?: anchor.BN,
): Promise<TxDataUpload> {
  const upload = await initTxData(program, paState, authority, payload.length, expiresSlotOverride);
  const { uploadId, txData } = upload;
  const chunkSize = 700;
  for (let offset = 0; offset < payload.length; offset += chunkSize) {
    const chunk = payload.subarray(offset, Math.min(payload.length, offset + chunkSize));
    await program.methods
      .txdataWrite(uploadId, offset, chunk)
      .accountsPartial({ txData, authority: authority.publicKey })
      .signers([authority])
      .rpc();
  }
  return upload;
}

/** Any 32 bytes that are not a real logic ref. */
export const randomRef = (): number[] => Array.from(Keypair.generate().publicKey.toBytes());

/** Wait for `sig` to reach confirmed commitment and fetch the transaction; fail if it is not fetchable then. */
export async function confirmedTransaction(
  connection: Connection,
  sig: string,
): Promise<anchor.web3.VersionedTransactionResponse> {
  await connection.confirmTransaction(sig, "confirmed");
  const tx = await connection.getTransaction(sig, { commitment: "confirmed", maxSupportedTransactionVersion: 1 });
  assert.ok(tx, `transaction ${sig} is fetchable once confirmed`);
  return tx!;
}

/** Resolve once the confirmed slot is past `targetSlot`; fail after `timeoutMs`. */
export async function waitForSlotPast(
  connection: Connection,
  targetSlot: number,
  timeoutMs: number = 30000,
): Promise<void> {
  const start = Date.now();
  while (Date.now() - start < timeoutMs) {
    const slot = await connection.getSlot("confirmed");
    if (slot > targetSlot) return;
    await new Promise((r) => setTimeout(r, 400));
  }
  throw new Error(`Timed out waiting for slot past ${targetSlot} after ${timeoutMs}ms`);
}

/** The heap frame settlement needs: bincode-deserializing an ARM transaction overruns the default 32 KiB. */
export const SETTLE_HEAP_FRAME_BYTES = 256 * 1024;

/** The compute limit a submitter's settlement transaction states (anoma-pa-solana-client's `SETTLE_COMPUTE_UNIT_LIMIT`). */
const SETTLE_COMPUTE_UNIT_LIMIT = 500_000;

/**
 * The account data a transaction may load: the runtime's maximum (64 MiB),
 * which legacy and v0 transactions get by default and a v1 transaction
 * states (unset, it is 0).
 */
const MAX_LOADED_ACCOUNTS_DATA_SIZE_BYTES = 64 * 1024 * 1024;

/** A web3.js instruction as @solana/kit's. */
function kitInstruction(ix: TransactionInstruction): Instruction {
  return {
    programAddress: address(ix.programId.toBase58()),
    accounts: ix.keys.map((key) => ({
      address: address(key.pubkey.toBase58()),
      role: key.isSigner
        ? key.isWritable
          ? AccountRole.WRITABLE_SIGNER
          : AccountRole.READONLY_SIGNER
        : key.isWritable
          ? AccountRole.WRITABLE
          : AccountRole.READONLY,
    })),
    data: new Uint8Array(ix.data),
  };
}

/**
 * Send `instructions` as one v1 transaction (SIMD-0385), the shape every
 * submitter sends a settlement in: the submitters' compute limit, the heap
 * frame settlement needs and the loaded-account-data limit in its header, paid by
 * the wallet and signed by it and `signers`. web3.js 1.x cannot serialize a v1
 * message, so @solana/kit compiles and signs it; web3.js sends its bytes, so a
 * refused transaction throws SendTransactionError with the runtime's logs, as
 * `provider.sendAndConfirm` does.
 */
export async function sendSettlementV1(
  provider: anchor.AnchorProvider,
  instructions: TransactionInstruction[],
  signers: Keypair[],
): Promise<string> {
  const payer = (provider.wallet as anchor.Wallet).payer;
  const { blockhash, lastValidBlockHeight } = await provider.connection.getLatestBlockhash("confirmed");
  const message = pipe(
    createTransactionMessage({ version: 1 }),
    (m) => setTransactionMessageFeePayer(address(payer.publicKey.toBase58()), m),
    (m) =>
      setTransactionMessageLifetimeUsingBlockhash(
        { blockhash: blockhash as Blockhash, lastValidBlockHeight: BigInt(lastValidBlockHeight) },
        m,
      ),
    (m) => setTransactionMessageComputeUnitLimit(SETTLE_COMPUTE_UNIT_LIMIT, m),
    (m) => setTransactionMessageHeapSize(SETTLE_HEAP_FRAME_BYTES, m),
    (m) => setTransactionMessageLoadedAccountsDataSizeLimit(MAX_LOADED_ACCOUNTS_DATA_SIZE_BYTES, m),
    (m) => appendTransactionMessageInstructions(instructions.map(kitInstruction), m),
  );
  const keyPairs = await Promise.all(
    [payer, ...signers.filter((s) => !s.publicKey.equals(payer.publicKey))].map((k) =>
      createKeyPairFromBytes(k.secretKey),
    ),
  );
  const signed = await signTransaction(keyPairs, compileTransaction(message));
  const signature = await provider.connection.sendRawTransaction(
    Buffer.from(getBase64EncodedWireTransaction(signed), "base64"),
  );
  const { value } = await provider.connection.confirmTransaction(
    { signature, blockhash, lastValidBlockHeight },
    "confirmed",
  );
  if (value.err) throw new Error(`settlement ${signature} failed: ${JSON.stringify(value.err)}`);
  return signature;
}

/**
 * pa-evm's transaction id: the Keccak-256 hash of the concatenated action
 * tree roots, the message the delta proof signs.
 */
export function transactionIdOf(actionTreeRoots: number[][]): Buffer {
  return Buffer.from(keccak_256(Buffer.concat(actionTreeRoots.map((root) => Buffer.from(root)))));
}

/** The Solana CLI against `provider`'s validator, signing with the suite's wallet, with JSON output. */
export function solanaCli(provider: anchor.AnchorProvider, ...args: string[]): string {
  const wallet = process.env.ANCHOR_WALLET;
  if (!wallet) throw new Error("solanaCli signs with ANCHOR_WALLET, which is unset (anchor-test.sh sets it)");
  return execFileSync(
    "solana",
    [...args, "--keypair", wallet, "--url", provider.connection.rpcEndpoint, "--output", "json"],
    { encoding: "utf8" },
  );
}

/** A loader buffer holding the program binary `so`, written by the suite's wallet, which is its authority. */
export function writeBuffer(provider: anchor.AnchorProvider, so: string): PublicKey {
  return new PublicKey((JSON.parse(solanaCli(provider, "program", "write-buffer", so)) as { buffer: string }).buffer);
}
