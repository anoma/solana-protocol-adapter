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
 *                                     close STF_TOKEN_MINT's escrow, close
 *                                     the config.
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
import {
  getAssociatedTokenAddress,
  getOrCreateAssociatedTokenAccount,
  TOKEN_PROGRAM_ID,
} from "@solana/spl-token";
import { ProtocolAdapter } from "../target/types/protocol_adapter";
import { SplTokenForwarder } from "../target/types/spl_token_forwarder";
import {
  NONCE_BITMAP_ACCOUNT_SIZE,
  OP_EMERGENCY_WITHDRAW,
} from "../tests/utils/constants";
import { encodeEmergencyWithdrawInput } from "../tests/utils/helpers";
import { deriveConfigPda, deriveEscrowPda, derivePaStatePda } from "../tests/utils/pda";

function fail(message: string): never {
  console.error(`❌ ${message}`);
  process.exit(1);
}

function requireEnv(name: string, what: string): string {
  const value = process.env[name];
  if (!value) fail(`Missing ${name}: ${what}`);
  return value;
}

function requirePubkey(name: string, what: string): PublicKey {
  const raw = requireEnv(name, what);
  try {
    return new PublicKey(raw);
  } catch {
    return fail(`${name} is not a valid pubkey: "${raw}"`);
  }
}

function requireLogicRef(): number[] {
  const hex = requireEnv(
    "STF_LOGIC_REF",
    "the 32-byte hex logic ref (verifying key) of the resource logic this forwarder serves"
  ).replace(/^0x/, "");
  if (!/^[0-9a-fA-F]{64}$/.test(hex)) fail(`STF_LOGIC_REF must be 64 hex chars, got "${hex}"`);
  return Array.from(Buffer.from(hex, "hex"));
}

function requireAmount(): bigint {
  const raw = requireEnv("STF_AMOUNT", "the amount to withdraw, in the token's raw units");
  if (!/^\d+$/.test(raw)) fail(`STF_AMOUNT must be a non-negative integer, got "${raw}"`);
  return BigInt(raw);
}

const provider = anchor.AnchorProvider.env();
anchor.setProvider(provider);
const wallet = provider.wallet as anchor.Wallet;
const connection = provider.connection;
const forwarder = anchor.workspace.SplTokenForwarder as Program<SplTokenForwarder>;
const adapter = anchor.workspace.ProtocolAdapter as Program<ProtocolAdapter>;
const [configPda] = deriveConfigPda(forwarder.programId);
const [paState] = derivePaStatePda(adapter.programId);

async function requireConfig() {
  try {
    return await forwarder.account.config.fetch(configPda);
  } catch {
    return fail(`forwarder config ${configPda.toBase58()} does not exist — run 'forwarder init' first`);
  }
}

async function requireCommittee() {
  const config = await requireConfig();
  if (!config.emergencyCommittee.equals(wallet.publicKey)) {
    fail(`wallet ${wallet.publicKey.toBase58()} is not the emergency committee (${config.emergencyCommittee.toBase58()})`);
  }
  return config;
}

async function escrowFor(mint: PublicKey) {
  const [escrowPda] = deriveEscrowPda(forwarder.programId, mint);
  const escrowAta = await getAssociatedTokenAddress(mint, escrowPda, true);
  return { escrowPda, escrowAta };
}

async function recipientAtaFor(mint: PublicKey, owner: PublicKey): Promise<PublicKey> {
  return (await getOrCreateAssociatedTokenAccount(connection, wallet.payer, mint, owner)).address;
}

function sol(lamports: number): string {
  return (lamports / LAMPORTS_PER_SOL).toFixed(6);
}

async function init() {
  const logicRef = requireLogicRef();
  const committee = requirePubkey("STF_EMERGENCY_COMMITTEE", "the committee that can name an emergency caller and close accounts");

  if (await connection.getAccountInfo(configPda)) {
    const config = await forwarder.account.config.fetch(configPda);
    console.log(`Config ${configPda.toBase58()} already initialized`);
    console.log(`  adapter:   ${config.protocolAdapter.toBase58()}`);
    console.log(`  logic ref: ${Buffer.from(config.logicRef).toString("hex")}`);
    console.log(`  committee: ${config.emergencyCommittee.toBase58()}`);
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
    const [escrowPda] = deriveEscrowPda(forwarder.programId, mint);
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
  const mint = requirePubkey("STF_TOKEN_MINT", "the mint whose escrow to withdraw from");
  const recipient = requirePubkey("STF_RECIPIENT", "the owner of the receiving token account");
  const amount = requireAmount();
  const config = await requireConfig();
  if (!config.emergencyCaller.equals(wallet.publicKey)) {
    fail(`wallet ${wallet.publicKey.toBase58()} is not the emergency caller (${config.emergencyCaller.toBase58()})`);
  }
  const { escrowPda, escrowAta } = await escrowFor(mint);
  const recipientAta = await recipientAtaFor(mint, recipient);

  await forwarder.methods
    .forwardEmergencyCall(encodeEmergencyWithdrawInput(OP_EMERGENCY_WITHDRAW, mint, amount, recipient))
    .accounts({ caller: wallet.publicKey, paState })
    .remainingAccounts([
      { pubkey: escrowAta, isSigner: false, isWritable: true },
      { pubkey: recipientAta, isSigner: false, isWritable: true },
      { pubkey: escrowPda, isSigner: false, isWritable: false },
      { pubkey: TOKEN_PROGRAM_ID, isSigner: false, isWritable: false },
    ])
    .rpc();
  console.log(`✅ Withdrew ${amount} raw units of ${mint.toBase58()} to ${recipientAta.toBase58()}`);
}

async function drainEscrow() {
  const mint = requirePubkey("STF_TOKEN_MINT", "the mint whose escrow to drain");
  const recipient = requirePubkey("STF_RECIPIENT", "the owner of the receiving token account");
  await requireCommittee();
  const { escrowPda, escrowAta } = await escrowFor(mint);
  if (!(await connection.getAccountInfo(escrowAta))) {
    fail(`escrow ATA ${escrowAta.toBase58()} does not exist; nothing to drain`);
  }
  const balance = (await connection.getTokenAccountBalance(escrowAta)).value;
  const recipientAta = await recipientAtaFor(mint, recipient);

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
  console.log(`✅ Drained ${balance.uiAmountString} of ${mint.toBase58()} to ${recipientAta.toBase58()} and closed the escrow ATA`);
}

async function teardown() {
  const mint = requirePubkey("STF_TOKEN_MINT", "the mint whose escrow to drain before closing the config");
  await requireCommittee();
  const before = await connection.getBalance(wallet.publicKey);

  const bitmaps = await connection.getProgramAccounts(forwarder.programId, {
    filters: [{ dataSize: NONCE_BITMAP_ACCOUNT_SIZE }],
  });
  console.log(`Closing ${bitmaps.length} nonce bitmap(s)`);
  const BATCH_SIZE = 20;
  for (let i = 0; i < bitmaps.length; i += BATCH_SIZE) {
    await forwarder.methods
      .closeNonceBitmapsBatch()
      .accountsPartial({ authority: wallet.publicKey, config: configPda })
      .remainingAccounts(
        bitmaps.slice(i, i + BATCH_SIZE).map(({ pubkey }) => ({ pubkey, isWritable: true, isSigner: false }))
      )
      .rpc();
  }

  const { escrowPda, escrowAta } = await escrowFor(mint);
  if (await connection.getAccountInfo(escrowAta)) {
    const recipientAta = await recipientAtaFor(mint, wallet.publicKey);
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
    console.log(`Escrow ${escrowAta.toBase58()} drained to ${recipientAta.toBase58()} and closed`);
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
