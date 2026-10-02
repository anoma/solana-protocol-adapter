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
 */
import * as anchor from "@anchor-lang/core";
import { Program } from "@anchor-lang/core";
import { PublicKey } from "@solana/web3.js";
import { getOrCreateAssociatedTokenAccount } from "@solana/spl-token";
import { ProtocolAdapter } from "../target/types/protocol_adapter";
import { SplTokenForwarder } from "../target/types/spl_token_forwarder";
import { emergencyWithdraw, escrowAccounts, initializeForwarder, reinitializeForwarder } from "../client/instructions";
import { deriveConfigPda, derivePaStatePda } from "../client/pda";
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

async function requireConfig() {
  const config = await forwarder.account.config.fetchNullable(configPda);
  return config ?? fail(`forwarder config ${configPda.toBase58()} does not exist — run 'forwarder init' first`);
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
  const recipient = requirePubkey("STF_RECIPIENT", "the owner of the receiving token account");
  const amount = requireRawAmount("STF_AMOUNT", "the amount to withdraw, in the token's raw units");
  await requireConfig();
  const { escrowAta } = escrowAccounts(forwarder.programId, mint);
  const recipientAta = (await getOrCreateAssociatedTokenAccount(connection, wallet.payer, mint, recipient)).address;

  await emergencyWithdraw(
    forwarder,
    paState,
    wallet.publicKey,
    { mint, amount, recipient },
    { escrowAta, recipientAta },
  ).rpc();
  console.log(`✅ Withdrew ${amount} raw units of ${mint.toBase58()} to ${recipientAta.toBase58()}`);
}

const COMMANDS: Record<string, () => Promise<void>> = {
  init,
  reinitialize: reinitializeCommand,
  "emergency-withdraw": withdraw,
};

const command = process.argv[2];
const run = command ? COMMANDS[command] : undefined;
if (!run) {
  fail(`unknown forwarder command "${command ?? ""}"; expected one of: ${Object.keys(COMMANDS).join(", ")}`);
}

console.log(`Forwarder: ${forwarder.programId.toBase58()}  Wallet: ${wallet.publicKey.toBase58()}`);
run().catch((err) => fail(`forwarder ${command} failed: ${err.message ?? err}`));
