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
 *   close-config          (committee) Close only the config PDA — the reset
 *                                     path for rotating the logic ref.
 *   set-emergency-caller  (committee) Name STF_EMERGENCY_CALLER, once, while
 *                                     the adapter is stopped.
 *   emergency-withdraw    (caller)    Move STF_AMOUNT of STF_TOKEN_MINT from
 *                                     escrow to STF_RECIPIENT.
 *   drain-escrow          (committee) Drain STF_TOKEN_MINT's escrow to
 *                                     STF_RECIPIENT and close the escrow ATA.
 *   teardown              (committee) Close every nonce bitmap, drain and
 *                                     close STF_TOKEN_MINT's escrow to the
 *                                     committee, close the config.
 *
 * Environment (read per command):
 *   STF_LOGIC_REF           32-byte hex: the resource logic the config
 *                           authorizes (init)
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
import { LAMPORTS_PER_SOL, PublicKey } from "@solana/web3.js";
import { getOrCreateAssociatedTokenAccount, TOKEN_PROGRAM_ID } from "@solana/spl-token";
import { ProtocolAdapter } from "../target/types/protocol_adapter";
import { SplTokenForwarder } from "../target/types/spl_token_forwarder";
import {
  closeAllNonceBitmaps,
  emergencyWithdrawAccounts,
  encodeUnwrapInput,
  escrowAccounts,
} from "../tests/utils/helpers";
import { deriveConfigPda, derivePaStatePda } from "../tests/utils/pda";
import { fail, requireEnv, requireHexBytes, requirePubkey } from "./cli-utils";

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

async function requireCommittee() {
  const config = await requireConfig();
  if (!config.emergencyCommittee.equals(wallet.publicKey)) {
    fail(`wallet ${wallet.publicKey.toBase58()} is not the emergency committee (${config.emergencyCommittee.toBase58()})`);
  }
  return config;
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
  await forwarder.methods
    .closeEscrow()
    .accountsPartial({
      authority: wallet.publicKey,
      config: configPda,
      escrowAta,
      escrowPda,
      recipientAta,
      tokenMint: mint,
      tokenProgram: TOKEN_PROGRAM_ID,
    })
    .rpc();
  console.log(`✅ Drained ${balance!.value.uiAmountString} of ${mint.toBase58()} to ${recipientAta.toBase58()} and closed the escrow ATA`);
}

function sol(lamports: number): string {
  return (lamports / LAMPORTS_PER_SOL).toFixed(6);
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
    await forwarder.methods
      .initialize(adapter.programId, logicRef, committee)
      .accounts({ authority: wallet.publicKey })
      .rpc();
    console.log("✅ Config initialized");
  }

  const mintRaw = process.env.STF_TOKEN_MINT;
  if (mintRaw) {
    const mint = new PublicKey(mintRaw);
    const { escrowPda } = escrowAccounts(forwarder.programId, mint);
    const escrowAta = await getOrCreateAssociatedTokenAccount(connection, wallet.payer, mint, escrowPda, true);
    console.log(`✅ Escrow for ${mint.toBase58()}: PDA ${escrowPda.toBase58()}, ATA ${escrowAta.address.toBase58()}`);
  }
}

async function closeConfig() {
  await requireCommittee();
  const before = await connection.getBalance(wallet.publicKey);
  await forwarder.methods
    .closeConfig()
    .accountsPartial({ authority: wallet.publicKey, config: configPda })
    .rpc();
  const after = await connection.getBalance(wallet.publicKey);
  console.log(`✅ Config ${configPda.toBase58()} closed, ${sol(after - before)} SOL net to the committee`);
}

async function setEmergencyCaller() {
  const caller = requirePubkey("STF_EMERGENCY_CALLER", "the key that will be allowed to withdraw from escrow");
  const config = await requireCommittee();
  if (!config.emergencyCaller.equals(PublicKey.default)) {
    fail(`emergency caller is already set to ${config.emergencyCaller.toBase58()} and cannot change`);
  }
  const state = await adapter.account.paStateAccount.fetch(paState);
  if (!("stopped" in state.lifecycle)) {
    fail("the adapter is running; the emergency caller can only be set after emergency_stop");
  }
  await forwarder.methods
    .setEmergencyCaller(caller)
    .accounts({ committee: wallet.publicKey, paState })
    .rpc();
  console.log(`✅ Emergency caller set to ${caller.toBase58()}`);
}

async function emergencyWithdraw() {
  const mint = requireMint();
  const recipient = requireRecipient();
  const rawAmount = requireEnv("STF_AMOUNT", "the amount to withdraw, in the token's raw units");
  if (!/^\d+$/.test(rawAmount)) fail(`STF_AMOUNT must be a non-negative integer, got "${rawAmount}"`);
  const amount = BigInt(rawAmount);
  const config = await requireConfig();
  if (!config.emergencyCaller.equals(wallet.publicKey)) {
    fail(`wallet ${wallet.publicKey.toBase58()} is not the emergency caller (${config.emergencyCaller.toBase58()})`);
  }
  const { escrowPda, escrowAta } = escrowAccounts(forwarder.programId, mint);
  const recipientAta = await recipientAtaFor(mint, recipient);

  await forwarder.methods
    .forwardEmergencyCall(encodeUnwrapInput(mint, amount, recipient, false))
    .accounts({ caller: wallet.publicKey, paState })
    .remainingAccounts(emergencyWithdrawAccounts(escrowAta, recipientAta, escrowPda))
    .rpc();
  console.log(`✅ Withdrew ${amount} raw units of ${mint.toBase58()} to ${recipientAta.toBase58()}`);
}

async function drainEscrow() {
  const mint = requireMint();
  const recipient = requireRecipient();
  await requireCommittee();
  await closeEscrowFor(mint, recipient);
}

async function teardown() {
  const mint = requireMint();
  await requireCommittee();
  const before = await connection.getBalance(wallet.publicKey);

  const closed = await closeAllNonceBitmaps(forwarder, configPda, wallet.publicKey, []);
  console.log(`Closed ${closed} nonce bitmap(s)`);

  const { escrowAta } = escrowAccounts(forwarder.programId, mint);
  if (await connection.getAccountInfo(escrowAta)) {
    await closeEscrowFor(mint, wallet.publicKey);
  } else {
    console.log(`Escrow ATA ${escrowAta.toBase58()} does not exist; skipping`);
  }

  await forwarder.methods
    .closeConfig()
    .accountsPartial({ authority: wallet.publicKey, config: configPda })
    .rpc();
  const after = await connection.getBalance(wallet.publicKey);
  console.log(`✅ Forwarder torn down, ${sol(after - before)} SOL net to the committee`);
}

const COMMANDS: Record<string, () => Promise<void>> = {
  init,
  "close-config": closeConfig,
  "set-emergency-caller": setEmergencyCaller,
  "emergency-withdraw": emergencyWithdraw,
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
