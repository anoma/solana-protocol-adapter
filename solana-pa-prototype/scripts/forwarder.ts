/**
 * SPL token forwarder operations. Run through ops.sh, which sets the
 * cluster and wallet:
 *
 *   ./scripts/dev.sh forwarder <command> --cluster <c> [--wallet <path>]
 *
 * Commands (the wallet that must sign is in parentheses):
 *   init                  (upgrade    Initialize the config and, with
 *                          authority) STF_TOKEN_MINT, the mint's escrow ATA.
 *                                     Idempotent; refuses an existing config
 *                                     that differs from the request.
 *   reinitialize          (upgrade    After upgrading the program to a
 *                          authority) build that raises CONFIG_VERSION:
 *                                     rotate the config's logic ref to
 *                                     STF_LOGIC_REF, once; escrow, nonce
 *                                     bitmaps and the committee are untouched.
 *   emergency-withdraw    (caller)    Move STF_AMOUNT of STF_TOKEN_MINT from
 *                                     escrow to STF_RECIPIENT.
 *   migrate               (upgrade    After upgrading the program in place
 *                          authority) from its previous build: migrate the
 *                                     config, every nonce bitmap still in the
 *                                     previous layout, and each
 *                                     STF_TOKEN_MINTS escrow. Idempotent.
 *
 * Closing escrows, nonce bitmaps or the config, and naming the emergency
 * caller, are not commands: on a live cluster they are done by hand
 * (docs/OPERATIONS.md).
 *
 * The program enforces who may do what and when; a refused command fails
 * with the program's error (UnauthorizedCaller, ProtocolAdapterNotPaused,
 * EmergencyCallerAlreadySet, ...).
 *
 * Environment (read per command):
 *   STF_LOGIC_REF           32-byte hex: the resource logic the config
 *                           authorizes (init, reinitialize)
 *   STF_EMERGENCY_COMMITTEE base58 pubkey (init)
 *   STF_TOKEN_MINT          base58 mint (init optional; emergency-withdraw
 *                           required)
 *   STF_RECIPIENT           base58 owner of the receiving token account
 *                           (emergency-withdraw)
 *   STF_AMOUNT              raw token units (emergency-withdraw)
 *   STF_TOKEN_MINTS         comma-separated base58 mints whose previous-build
 *                           escrow to move (migrate)
 */
import * as anchor from "@anchor-lang/core";
import { Program } from "@anchor-lang/core";
import { PublicKey } from "@solana/web3.js";
import { getOrCreateAssociatedTokenAccount } from "@solana/spl-token";
import { ProtocolAdapter } from "../target/types/protocol_adapter";
import { SplTokenForwarder } from "../target/types/spl_token_forwarder";
import {
  emergencyWithdraw,
  escrowAccounts,
  initializeForwarder,
  migrateConfig,
  migrateEscrow,
  migrateNonceBitmap,
  previousEscrowAccounts,
  reinitializeForwarder,
} from "../client/instructions";
import { PREVIOUS_CONFIG_SIZE, PREVIOUS_NONCE_BITMAP_SIZE } from "../client/constants";
import { deriveConfigPda, deriveNonceBitmapPda, derivePaStatePda } from "../client/pda";
import { fail, pubkeyList, requireHexBytes, requirePubkey, requireRawAmount } from "./cli-utils";

const provider = anchor.AnchorProvider.env();
anchor.setProvider(provider);
const wallet = provider.wallet as anchor.Wallet;
const connection = provider.connection;
const forwarder = anchor.workspace.SplTokenForwarder as Program<SplTokenForwarder>;
const adapter = anchor.workspace.ProtocolAdapter as Program<ProtocolAdapter>;
const [configPda] = deriveConfigPda(forwarder.programId);
const [paState] = derivePaStatePda(adapter.programId);

const requireMint = () => requirePubkey("STF_TOKEN_MINT", "the mint whose escrow to operate on");
const requireRecipient = () => requirePubkey("STF_RECIPIENT", "the owner of the receiving token account");

async function requireConfig() {
  const config = await forwarder.account.config.fetchNullable(configPda);
  return config ?? fail(`forwarder config ${configPda.toBase58()} does not exist — run 'forwarder init' first`);
}

async function recipientAtaFor(mint: PublicKey, owner: PublicKey): Promise<PublicKey> {
  return (await getOrCreateAssociatedTokenAccount(connection, wallet.payer, mint, owner)).address;
}

async function init() {
  const logicRef = requireHexBytes(
    "STF_LOGIC_REF",
    32,
    "the 32-byte hex logic ref (verifying key) of the resource logic this forwarder serves",
  );
  const committee = requirePubkey(
    "STF_EMERGENCY_COMMITTEE",
    "the committee that can name an emergency caller and close accounts",
  );

  const existing = await forwarder.account.config.fetchNullable(configPda);
  if (existing) {
    // (field, stored, requested)
    const fields: [string, string, string][] = [
      ["adapter", existing.protocolAdapter.toBase58(), adapter.programId.toBase58()],
      ["logic ref", Buffer.from(existing.logicRef).toString("hex"), Buffer.from(logicRef).toString("hex")],
      ["committee", existing.emergencyCommittee.toBase58(), committee.toBase58()],
    ];
    const mismatches = fields.filter(([, stored, requested]) => stored !== requested);
    if (mismatches.length > 0) {
      fail(
        `config ${configPda.toBase58()} already exists with a different ${mismatches.map(([k]) => k).join(", ")}: ` +
          mismatches.map(([k, stored, requested]) => `${k} ${stored} (requested ${requested})`).join("; "),
      );
    }
    console.log(`Config ${configPda.toBase58()} already initialized with the requested values`);
  } else {
    console.log(`Initializing config ${configPda.toBase58()}`);
    console.log(`  adapter:   ${adapter.programId.toBase58()}`);
    console.log(`  logic ref: ${Buffer.from(logicRef).toString("hex")}`);
    console.log(`  committee: ${committee.toBase58()}`);
    await initializeForwarder(forwarder, adapter.programId, logicRef, committee, wallet.publicKey).rpc();
    console.log("✅ Config initialized");
  }

  if (process.env.STF_TOKEN_MINT) {
    const mint = requireMint();
    const { escrowAuthority } = escrowAccounts(forwarder.programId, mint);
    const escrowAta = await getOrCreateAssociatedTokenAccount(connection, wallet.payer, mint, escrowAuthority, true);
    console.log(
      `✅ Escrow for ${mint.toBase58()}: authority ${escrowAuthority.toBase58()}, ATA ${escrowAta.address.toBase58()}`,
    );
  }
}

async function reinitializeCommand() {
  const logicRef = requireHexBytes(
    "STF_LOGIC_REF",
    32,
    "the 32-byte hex logic ref (verifying key) the config should authorize from now on",
  );
  const existing = await requireConfig();
  const previous = Buffer.from(existing.logicRef).toString("hex");
  await reinitializeForwarder(forwarder, wallet.publicKey, logicRef).rpc();
  const config = await requireConfig();
  console.log(
    `✅ Logic ref rotated: ${previous} -> ${Buffer.from(config.logicRef).toString("hex")} (config version ${config.version})`,
  );
}

async function withdraw() {
  const mint = requireMint();
  const recipient = requireRecipient();
  const amount = requireRawAmount("STF_AMOUNT", "the amount to withdraw, in the token's raw units");
  await requireConfig();
  const { escrowAta } = escrowAccounts(forwarder.programId, mint);
  const recipientAta = await recipientAtaFor(mint, recipient);

  await emergencyWithdraw(
    forwarder,
    paState,
    wallet.publicKey,
    { mint, amount, recipient },
    { escrowAta, recipientAta },
  ).rpc();
  console.log(`✅ Withdrew ${amount} raw units of ${mint.toBase58()} to ${recipientAta.toBase58()}`);
}

const instructionCoder = new anchor.BorshInstructionCoder(forwarder.idl);

/**
 * The (user, word index) a previous-layout bitmap belongs to: a bitmap
 * stores only its bits, and its address is a PDA of those two, so they are
 * read from the init_nonce_bitmap instruction that created it, its oldest
 * transaction.
 */

async function bitmapOwner(bitmap: PublicKey): Promise<{ user: PublicKey; wordIndex: bigint }> {
  let oldest: string | undefined;
  for (let before: string | undefined; ;) {
    const page = await connection.getSignaturesForAddress(bitmap, { before, limit: 1000 }, "confirmed");
    if (page.length === 0) break;
    oldest = page[page.length - 1].signature;
    before = oldest;
  }
  if (!oldest) throw new Error(`nonce bitmap ${bitmap.toBase58()} has no transactions`);
  const tx = await connection.getTransaction(oldest, {
    commitment: "confirmed",
    maxSupportedTransactionVersion: 0,
  });
  if (!tx) throw new Error(`transaction ${oldest} that created ${bitmap.toBase58()} is not available`);
  const keys = tx.transaction.message.getAccountKeys({ accountKeysFromLookups: tx.meta?.loadedAddresses });
  const instructions: { programIdIndex: number; accountKeyIndexes: number[]; data: string }[] = [
    ...tx.transaction.message.compiledInstructions.map((ix) => ({
      programIdIndex: ix.programIdIndex,
      accountKeyIndexes: ix.accountKeyIndexes,
      data: anchor.utils.bytes.bs58.encode(ix.data),
    })),
    ...(tx.meta?.innerInstructions ?? []).flatMap((inner) =>
      inner.instructions.map((ix) => ({
        programIdIndex: ix.programIdIndex,
        accountKeyIndexes: ix.accounts,
        data: ix.data,
      })),
    ),
  ];
  for (const ix of instructions) {
    if (!keys.get(ix.programIdIndex)!.equals(forwarder.programId)) continue;
    const decoded = instructionCoder.decode(ix.data, "base58");
    if (decoded?.name !== "initNonceBitmap") continue;
    const { user, wordIndex: word } = decoded.data as { user: PublicKey; wordIndex: anchor.BN };
    const wordIndex = BigInt(word.toString());
    if (deriveNonceBitmapPda(forwarder.programId, user, wordIndex)[0].equals(bitmap)) return { user, wordIndex };
  }
  throw new Error(`transaction ${oldest}, the oldest touching ${bitmap.toBase58()}, has no init_nonce_bitmap for it`);
}

async function migrate() {
  const mints = pubkeyList("STF_TOKEN_MINTS");

  const config = await connection.getAccountInfo(configPda);
  if (!config) fail(`forwarder config ${configPda.toBase58()} does not exist`);
  if (config.data.length === PREVIOUS_CONFIG_SIZE) {
    await migrateConfig(forwarder, wallet.publicKey).rpc();
    console.log(`✅ Migrated config ${configPda.toBase58()}`);
  } else {
    console.log(`Config ${configPda.toBase58()} is already in this build's layout`);
  }

  const previousBitmaps = await connection.getProgramAccounts(forwarder.programId, {
    filters: [{ dataSize: PREVIOUS_NONCE_BITMAP_SIZE }, { memcmp: forwarder.coder.accounts.memcmp("nonceBitmap") }],
  });
  for (const { pubkey } of previousBitmaps) {
    const { user, wordIndex } = await bitmapOwner(pubkey);
    await migrateNonceBitmap(forwarder, wallet.publicKey, user, wordIndex).rpc();
    console.log(`✅ Migrated nonce bitmap ${pubkey.toBase58()} (user ${user.toBase58()}, word ${wordIndex})`);
  }
  console.log(`${previousBitmaps.length} nonce bitmap(s) were in the previous layout`);

  for (const mint of mints) {
    const { previousEscrowAta } = previousEscrowAccounts(forwarder.programId, mint);
    if ((await connection.getAccountInfo(previousEscrowAta)) === null) {
      console.log(`${mint.toBase58()} has no previous-build escrow ${previousEscrowAta.toBase58()}`);
      continue;
    }
    const { escrowAuthority } = escrowAccounts(forwarder.programId, mint);
    await getOrCreateAssociatedTokenAccount(connection, wallet.payer, mint, escrowAuthority, true);
    const { value } = await connection.getTokenAccountBalance(previousEscrowAta);
    await migrateEscrow(forwarder, wallet.publicKey, mint).rpc();
    console.log(
      `✅ Moved ${value.uiAmountString} of ${mint.toBase58()} to the escrow authority; closed ${previousEscrowAta.toBase58()}`,
    );
  }
}

const COMMANDS: Record<string, () => Promise<void>> = {
  init,
  reinitialize: reinitializeCommand,
  "emergency-withdraw": withdraw,
  migrate,
};

const command = process.argv[2];
const run = command ? COMMANDS[command] : undefined;
if (!run) {
  fail(`unknown forwarder command "${command ?? ""}"; expected one of: ${Object.keys(COMMANDS).join(", ")}`);
}

console.log(`Forwarder: ${forwarder.programId.toBase58()}  Wallet: ${wallet.publicKey.toBase58()}`);
run().catch((err) => fail(`forwarder ${command} failed: ${err.message ?? err}`));
