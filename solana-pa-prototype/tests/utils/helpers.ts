/**
 * Test-suite helpers: funding and draining keypairs, error assertions,
 * TxData uploads, fixture actors, and v0 transaction sending.
 */
import * as anchor from "@anchor-lang/core";
import { Idl, Program } from "@anchor-lang/core";
import {
  AddressLookupTableAccount,
  Connection,
  Keypair,
  LAMPORTS_PER_SOL,
  MessageV0,
  PublicKey,
  SendTransactionError,
  SystemProgram,
  Transaction,
  TransactionInstruction,
  TransactionMessage,
  VersionedTransaction,
} from "@solana/web3.js";
import { approve, createMint, getOrCreateAssociatedTokenAccount, mintTo } from "@solana/spl-token";
import { assert } from "chai";
import { createHash } from "crypto";
import { ProtocolAdapter } from "../../target/types/protocol_adapter";
import { escrowAccounts } from "../../client/instructions";
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
  return {
    async fund(kp: Keypair, sol: number) {
      await fundKeypair(provider, kp, sol);
      funded.push(kp);
    },
    async drainAll() {
      const drained = await drainKeypairs(provider, funded);
      funded.length = 0;
      return drained;
    },
  };
}

/**
 * A fresh party's token account of `mint`, holding `amount` minted by
 * `mintAuthority` and approving `delegate` for all of it: an account a
 * submitter could name in a transfer although its owner signed nothing.
 */
export async function approvedTokenAccount(
  connection: Connection,
  funder: ReturnType<typeof makeFunder>,
  mint: PublicKey,
  mintAuthority: Keypair,
  delegate: PublicKey,
  amount: number | bigint,
): Promise<PublicKey> {
  const owner = Keypair.generate();
  await funder.fund(owner, 1);
  const ata = (await getOrCreateAssociatedTokenAccount(connection, owner, mint, owner.publicKey)).address;
  await mintTo(connection, mintAuthority, mint, ata, mintAuthority, amount);
  await approve(connection, owner, ata, delegate, owner, amount);
  return ata;
}

/** A keypair every test file can rebuild from the same label. */
export function seededKeypair(label: string): Keypair {
  return Keypair.fromSeed(createHash("sha256").update(label).digest());
}

/**
 * How a transaction failed, read from its logs: the program whose frame
 * raised the error and the reason that frame's `Program <id> failed: <reason>`
 * line gives. The raising frame is the innermost failing one: a failed CPI
 * logs its own failure line first, and every caller frame then repeats the
 * same error on its own line. `frameLogs` are the lines the raising frame
 * logged itself, without its callees' lines.
 */
type TransactionFailure = { program: PublicKey; reason: string; frameLogs: string[] };

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
      return { program: new PublicKey(failed[1]), reason: failed[2], frameLogs: frame.logs };
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
  assert.equal(failure.program.toBase58(), program.toBase58(), `failing program${context}`);
  assert.match(failure.reason, reason, `failure reason of ${program.toBase58()}${context}`);
  if ("error" in expected && expected.account !== undefined) {
    const origin = failure.frameLogs
      .map((l) => /^Program log: AnchorError caused by account: (\w+)\./.exec(l)?.[1])
      .find((a) => a !== undefined);
    assert.equal(origin, expected.account, `account the error names${context}`);
  }
}

/**
 * A TxData upload id and its little-endian seed bytes. The id is the wall
 * clock so consecutive uploads by one authority never collide.
 */
export function freshUploadId(): { uploadId: anchor.BN; uploadIdLe: Buffer } {
  const uploadId = new anchor.BN(Date.now());
  const uploadIdLe = Buffer.alloc(8);
  uploadIdLe.writeBigUInt64LE(BigInt(uploadId.toString()));
  return { uploadId, uploadIdLe };
}

/**
 * Create an empty TxData account of `payloadSize` bytes under `authority`,
 * expiring 10,000 slots from now unless `expiresSlotOverride` is given.
 */
export async function initTxData(
  program: Program<ProtocolAdapter>,
  paState: PublicKey,
  authority: Keypair,
  payloadSize: number,
  expiresSlotOverride?: anchor.BN,
): Promise<{ uploadId: anchor.BN; uploadIdLe: Buffer; txData: PublicKey; expiresSlot: anchor.BN }> {
  const { uploadId, uploadIdLe } = freshUploadId();
  const txData = deriveTxDataPda(program.programId, authority.publicKey, uploadIdLe);
  const expiresSlot =
    expiresSlotOverride ?? new anchor.BN((await program.provider.connection.getSlot("confirmed")) + 10_000);
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

/** Create a TxData account under `authority` and write `payload` into it in 700-byte chunks. */
export async function uploadTxData(
  program: Program<ProtocolAdapter>,
  paState: PublicKey,
  authority: Keypair,
  payload: Buffer,
  expiresSlotOverride?: anchor.BN,
): Promise<{ uploadId: anchor.BN; uploadIdLe: Buffer; txData: PublicKey; expiresSlot: anchor.BN }> {
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

/**
 * A fresh 6-decimal mint with `payer` as its authority, its escrow ATA
 * created and holding `amount` raw units.
 */
export async function createFundedEscrow(
  provider: anchor.AnchorProvider,
  forwarderProgramId: PublicKey,
  payer: Keypair,
  amount: bigint,
): Promise<{ mint: PublicKey; escrowPda: PublicKey; escrowAta: PublicKey }> {
  const mint = await createMint(provider.connection, payer, payer.publicKey, null, 6);
  const { escrowPda, escrowAta } = escrowAccounts(forwarderProgramId, mint);
  await getOrCreateAssociatedTokenAccount(provider.connection, payer, mint, escrowPda, true);
  await mintTo(provider.connection, payer, mint, escrowAta, payer, Number(amount));
  return { mint, escrowPda, escrowAta };
}

/** Any 32 bytes that are not a real logic ref. */
export const randomRef = (): number[] => Array.from(Keypair.generate().publicKey.toBytes());

/** Wait for `sig` to reach confirmed commitment and fetch the transaction; fail if it is not fetchable then. */
export async function confirmedTransaction(
  connection: Connection,
  sig: string,
): Promise<anchor.web3.VersionedTransactionResponse> {
  await connection.confirmTransaction(sig, "confirmed");
  const tx = await connection.getTransaction(sig, { commitment: "confirmed", maxSupportedTransactionVersion: 0 });
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

/** Compile `instructions` into a v0 message against `table`, the provider wallet paying. */
export async function compileV0(
  provider: anchor.AnchorProvider,
  instructions: TransactionInstruction[],
  table: AddressLookupTableAccount,
): Promise<MessageV0> {
  const { blockhash } = await provider.connection.getLatestBlockhash("confirmed");
  return new TransactionMessage({
    payerKey: provider.wallet.publicKey,
    recentBlockhash: blockhash,
    instructions,
  }).compileToV0Message([table]);
}

/**
 * Send `instructions` as a v0 transaction against `table`, signed by the
 * wallet and `signers`. The provider wraps a failed transaction's logs into
 * the thrown error as it does for legacy transactions.
 */
export async function sendV0(
  provider: anchor.AnchorProvider,
  instructions: TransactionInstruction[],
  signers: Keypair[],
  table: AddressLookupTableAccount,
): Promise<string> {
  const message = await compileV0(provider, instructions, table);
  return provider.sendAndConfirm(new VersionedTransaction(message), signers);
}
