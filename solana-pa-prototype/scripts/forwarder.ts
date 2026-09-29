/**
 * SPL token forwarder operations. Run through ops.sh, which sets the
 * cluster and wallet:
 *
 *   ./scripts/dev.sh forwarder <command> --cluster <c> [--wallet <path>]
 *
 * Commands (the wallet that must sign is in parentheses):
 *   init                  (deployer)  Initialize the config and, with
 *                                     STF_TOKEN_MINT, the mint's escrow ATA.
 *                                     Idempotent.
 *   set-logic-ref         (upgrade    Rotate the config's logic ref to
 *                          authority) STF_LOGIC_REF in place; escrow, nonce
 *                                     bitmaps and the committee are untouched.
 *   close-config          (committee) Close only the config PDA (retirement).
 *                                     Requires the adapter to be stopped.
 *   set-emergency-caller  (committee) Name STF_EMERGENCY_CALLER, once, while
 *                                     the adapter is stopped.
 *   emergency-withdraw    (caller)    Move STF_AMOUNT of STF_TOKEN_MINT from
 *                                     escrow to STF_RECIPIENT.
 *   drain-escrow          (committee) Drain STF_TOKEN_MINT's escrow to
 *                                     STF_RECIPIENT and close the escrow ATA.
 *                                     Requires the adapter to be stopped.
 *   teardown              (committee) Close every nonce bitmap, drain and
 *                                     close STF_TOKEN_MINT's escrow to the
 *                                     committee, close the config. Requires
 *                                     the adapter to be stopped.
 *
 * The program enforces who may do what and when; a refused command fails
 * with the program's error (UnauthorizedCaller, ProtocolAdapterNotStopped,
 * EmergencyCallerAlreadySet, ...).
 *
 * Environment (read per command):
 *   STF_LOGIC_REF           32-byte hex: the resource logic the config
 *                           authorizes (init, set-logic-ref)
 *   STF_EMERGENCY_COMMITTEE base58 pubkey (init)
 *   STF_TOKEN_MINT          base58 mint (init optional; emergency-withdraw,
 *                           drain-escrow, teardown required)
 *   STF_EMERGENCY_CALLER    base58 pubkey (set-emergency-caller)
 *   STF_RECIPIENT           base58 owner of the receiving token account
 *                           (emergency-withdraw, drain-escrow)
 *   STF_AMOUNT              raw token units (emergency-withdraw)
 */
import * as anchor from "@coral-xyz/anchor";
import { Program } from "@coral-xyz/anchor";
import { PublicKey } from "@solana/web3.js";
import { getOrCreateAssociatedTokenAccount } from "@solana/spl-token";
import { ProtocolAdapter } from "../target/types/protocol_adapter";
import { SplTokenForwarder } from "../target/types/spl_token_forwarder";
import {
  closeAllNonceBitmaps,
  closeConfig,
  closeEscrow,
  emergencyWithdraw,
  escrowAccounts,
  initializeForwarder,
  setEmergencyCaller,
  setLogicRef,
} from "../tests/utils/helpers";
import { deriveConfigPda, derivePaStatePda } from "../tests/utils/pda";
import { fail, requireHexBytes, requirePubkey, requireRawAmount } from "./cli-utils";

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

/** Drain a mint's escrow to `recipientOwner` and close the escrow ATA, as the committee. */
async function closeEscrowFor(mint: PublicKey, recipientOwner: PublicKey) {
  const { escrowPda, escrowAta } = escrowAccounts(forwarder.programId, mint);
  const balance = await connection.getTokenAccountBalance(escrowAta).catch(() => null);
  if (!balance) fail(`escrow ATA ${escrowAta.toBase58()} does not exist; nothing to drain`);
  const recipientAta = await recipientAtaFor(mint, recipientOwner);
  await closeEscrow(forwarder, wallet.publicKey, paState, { mint, escrowPda, escrowAta, recipientAta }).rpc();
  console.log(`✅ Drained ${balance!.value.uiAmountString} of ${mint.toBase58()} to ${recipientAta.toBase58()} and closed the escrow ATA`);
}

async function init() {
  const logicRef = requireHexBytes(
    "STF_LOGIC_REF",
    32,
    "the 32-byte hex logic ref (verifying key) of the resource logic this forwarder serves"
  );
  const committee = requirePubkey("STF_EMERGENCY_COMMITTEE", "the committee that can name an emergency caller and close accounts");

  const existing = await forwarder.account.config.fetchNullable(configPda);
  if (existing) {
    console.log(`Config ${configPda.toBase58()} already initialized`);
    console.log(`  adapter:   ${existing.protocolAdapter.toBase58()}`);
    console.log(`  logic ref: ${Buffer.from(existing.logicRef).toString("hex")}`);
    console.log(`  committee: ${existing.emergencyCommittee.toBase58()}`);
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
    const { escrowPda } = escrowAccounts(forwarder.programId, mint);
    const escrowAta = await getOrCreateAssociatedTokenAccount(connection, wallet.payer, mint, escrowPda, true);
    console.log(`✅ Escrow for ${mint.toBase58()}: PDA ${escrowPda.toBase58()}, ATA ${escrowAta.address.toBase58()}`);
  }
}

async function closeConfigCommand() {
  await requireConfig();
  await closeConfig(forwarder, wallet.publicKey, paState).rpc();
  console.log(`✅ Config ${configPda.toBase58()} closed`);
}

async function setLogicRefCommand() {
  const logicRef = requireHexBytes("STF_LOGIC_REF", 32, "the 32-byte hex logic ref (verifying key) the config should authorize from now on");
  const existing = await requireConfig();
  const previous = Buffer.from(existing.logicRef).toString("hex");
  await setLogicRef(forwarder, wallet.publicKey, logicRef).rpc();
  console.log(`✅ Logic ref rotated: ${previous} -> ${Buffer.from(logicRef).toString("hex")}`);
}

async function setEmergencyCallerCommand() {
  const caller = requirePubkey("STF_EMERGENCY_CALLER", "the key that will be allowed to withdraw from escrow");
  await requireConfig();
  await setEmergencyCaller(forwarder, wallet.publicKey, paState, caller).rpc();
  console.log(`✅ Emergency caller set to ${caller.toBase58()}`);
}

async function withdraw() {
  const mint = requireMint();
  const recipient = requireRecipient();
  const amount = requireRawAmount("STF_AMOUNT", "the amount to withdraw, in the token's raw units");
  await requireConfig();
  const { escrowPda, escrowAta } = escrowAccounts(forwarder.programId, mint);
  const recipientAta = await recipientAtaFor(mint, recipient);

  await emergencyWithdraw(forwarder, paState, wallet.publicKey, { mint, amount, recipient }, { escrowAta, recipientAta, escrowPda }).rpc();
  console.log(`✅ Withdrew ${amount} raw units of ${mint.toBase58()} to ${recipientAta.toBase58()}`);
}

async function drainEscrow() {
  const mint = requireMint();
  const recipient = requireRecipient();
  await requireConfig();
  await closeEscrowFor(mint, recipient);
}

async function teardown() {
  const mint = requireMint();
  await requireConfig();

  const closed = await closeAllNonceBitmaps(forwarder, wallet.publicKey, paState, []);
  console.log(`Closed ${closed} nonce bitmap(s)`);

  const { escrowAta } = escrowAccounts(forwarder.programId, mint);
  if (await connection.getAccountInfo(escrowAta)) {
    await closeEscrowFor(mint, wallet.publicKey);
  } else {
    console.log(`Escrow ATA ${escrowAta.toBase58()} does not exist; skipping`);
  }

  await closeConfigCommand();
}

const COMMANDS: Record<string, () => Promise<void>> = {
  init,
  "set-logic-ref": setLogicRefCommand,
  "close-config": closeConfigCommand,
  "set-emergency-caller": setEmergencyCallerCommand,
  "emergency-withdraw": withdraw,
  "drain-escrow": drainEscrow,
  teardown,
};

const command = process.argv[2];
const run = command ? COMMANDS[command] : undefined;
if (!run) {
  fail(`unknown forwarder command "${command ?? ""}"; expected one of: ${Object.keys(COMMANDS).join(", ")}`);
}

console.log(`Forwarder: ${forwarder.programId.toBase58()}  Wallet: ${wallet.publicKey.toBase58()}`);
run().catch((err) => fail(`forwarder ${command} failed: ${err.message ?? err}`));
