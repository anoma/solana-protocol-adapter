/**
 * Close only the SPL Token Forwarder Config PDA.
 *
 * This intentionally does not close nonce bitmaps or escrow accounts. It is the
 * narrow reset path for replacing the config's authorized logic_ref.
 */
import * as anchor from "@coral-xyz/anchor";
import { Keypair, PublicKey, LAMPORTS_PER_SOL } from "@solana/web3.js";
import { readFileSync } from "fs";
import path from "path";
import { deriveConfigPda } from "../tests/utils/pda";

const CONFIG_DISCRIMINATOR_LEN = 8;
const PUBKEY_LEN = 32;
const LOGIC_REF_LEN = 32;

function loadProgramId(name: string): PublicKey {
  const keypairPath = path.resolve(
    __dirname,
    "..",
    "target",
    "deploy",
    `${name}-keypair.json`
  );
  const keypairData = JSON.parse(readFileSync(keypairPath, "utf8"));
  return Keypair.fromSecretKey(Uint8Array.from(keypairData)).publicKey;
}

function configFieldOffsets() {
  const protocolAdapter = CONFIG_DISCRIMINATOR_LEN;
  const logicRef = protocolAdapter + PUBKEY_LEN;
  const emergencyCommittee = logicRef + LOGIC_REF_LEN;
  const emergencyCaller = emergencyCommittee + PUBKEY_LEN;
  const bump = emergencyCaller + PUBKEY_LEN;
  return {
    protocolAdapter,
    logicRef,
    emergencyCommittee,
    emergencyCaller,
    bump,
  };
}

function readPubkey(data: Buffer, offset: number): PublicKey {
  return new PublicKey(data.subarray(offset, offset + PUBKEY_LEN));
}

function readLogicRefHex(data: Buffer, offset: number): string {
  return data.subarray(offset, offset + LOGIC_REF_LEN).toString("hex");
}

async function main() {
  const dryRun = process.argv.includes("--dry-run");
  const targetLogicRef = process.env.TARGET_LOGIC_REF;

  if (!targetLogicRef || !/^[0-9a-fA-F]{64}$/.test(targetLogicRef)) {
    throw new Error("TARGET_LOGIC_REF env var is required (64 hex chars)");
  }

  const provider = anchor.AnchorProvider.env();
  anchor.setProvider(provider);

  const wallet = provider.wallet as anchor.Wallet;
  const connection = provider.connection;
  const forwarderProgramId = loadProgramId("spl_token_forwarder");
  const expectedForwarderProgramId = process.env.EXPECTED_FORWARDER_PROGRAM_ID;
  if (
    expectedForwarderProgramId &&
    forwarderProgramId.toBase58() !== expectedForwarderProgramId
  ) {
    throw new Error(
      `target/deploy/spl_token_forwarder-keypair.json resolves to ${forwarderProgramId.toBase58()}, ` +
        `but EXPECTED_FORWARDER_PROGRAM_ID is ${expectedForwarderProgramId}`
    );
  }

  const idlPath = path.resolve(
    __dirname,
    "..",
    "target",
    "idl",
    "spl_token_forwarder.json"
  );
  const idl = JSON.parse(readFileSync(idlPath, "utf8"));
  idl.address = forwarderProgramId.toBase58();
  const program = new anchor.Program(idl, provider);

  const [configPda] = deriveConfigPda(forwarderProgramId);
  const configInfo = await connection.getAccountInfo(configPda);

  console.log("Forwarder:", forwarderProgramId.toBase58());
  console.log("Config PDA:", configPda.toBase58());
  console.log("Wallet:   ", wallet.publicKey.toBase58());
  console.log("Target logic_ref:", targetLogicRef.toLowerCase());

  if (!configInfo) {
    console.log("Config not found; nothing to close.");
    return;
  }

  const offsets = configFieldOffsets();
  const protocolAdapter = readPubkey(configInfo.data, offsets.protocolAdapter);
  const logicRef = readLogicRefHex(configInfo.data, offsets.logicRef);
  const emergencyCommittee = readPubkey(
    configInfo.data,
    offsets.emergencyCommittee
  );
  const emergencyCaller = readPubkey(configInfo.data, offsets.emergencyCaller);
  const bump = configInfo.data[offsets.bump];

  console.log("Current protocol_adapter:", protocolAdapter.toBase58());
  console.log("Current logic_ref:       ", logicRef);
  console.log("Emergency committee:     ", emergencyCommittee.toBase58());
  console.log("Emergency caller:        ", emergencyCaller.toBase58());
  console.log("Bump:                    ", bump);
  console.log(
    "Config rent:             ",
    `${(configInfo.lamports / LAMPORTS_PER_SOL).toFixed(6)} SOL`
  );

  if (logicRef.toLowerCase() === targetLogicRef.toLowerCase()) {
    console.log("Config already has the target logic_ref; no close needed.");
    return;
  }

  if (!emergencyCommittee.equals(wallet.publicKey)) {
    throw new Error(
      `Wallet ${wallet.publicKey.toBase58()} is not the emergency committee ` +
        `(${emergencyCommittee.toBase58()})`
    );
  }

  if (dryRun) {
    console.log("Dry run: config would be closed.");
    return;
  }

  const tx = await program.methods
    .closeConfig()
    .accounts({
      authority: wallet.publicKey,
      config: configPda,
    })
    .rpc();

  console.log("Config closed.");
  console.log("Signature:", tx);
}

main().catch((err) => {
  console.error("Close forwarder config failed:", err);
  process.exit(1);
});
