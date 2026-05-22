/**
 * Drain one SPL Token Forwarder escrow to an explicit recipient and recreate it
 * separately with init-forwarder.
 */
import * as anchor from "@coral-xyz/anchor";
import { Keypair, PublicKey } from "@solana/web3.js";
import {
  getAssociatedTokenAddress,
  getOrCreateAssociatedTokenAccount,
  TOKEN_PROGRAM_ID,
} from "@solana/spl-token";
import { readFileSync } from "fs";
import path from "path";
import { deriveConfigPda, deriveEscrowPda } from "../tests/utils/pda";

function requiredEnv(name: string): string {
  const value = process.env[name];
  if (!value) {
    throw new Error(`${name} env var is required`);
  }
  return value;
}

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

async function main() {
  const provider = anchor.AnchorProvider.env();
  anchor.setProvider(provider);

  const wallet = provider.wallet as anchor.Wallet;
  const connection = provider.connection;
  const tokenMint = new PublicKey(requiredEnv("TOKEN_MINT"));
  const recipientOwner = new PublicKey(requiredEnv("RECIPIENT_OWNER"));
  const expectedForwarder = process.env.EXPECTED_FORWARDER_PROGRAM_ID;

  const forwarderProgramId = loadProgramId("spl_token_forwarder");
  if (
    expectedForwarder &&
    forwarderProgramId.toBase58() !== expectedForwarder
  ) {
    throw new Error(
      `forwarder program ${forwarderProgramId.toBase58()} does not match EXPECTED_FORWARDER_PROGRAM_ID ${expectedForwarder}`
    );
  }

  const forwarderIdlPath = path.resolve(
    __dirname,
    "..",
    "target",
    "idl",
    "spl_token_forwarder.json"
  );
  const forwarderIdl = JSON.parse(readFileSync(forwarderIdlPath, "utf8"));
  forwarderIdl.address = forwarderProgramId.toBase58();
  const program = new anchor.Program(forwarderIdl, provider);

  const [configPda] = deriveConfigPda(forwarderProgramId);
  const configInfo = await connection.getAccountInfo(configPda);
  if (!configInfo) {
    throw new Error(`forwarder config ${configPda.toBase58()} does not exist`);
  }

  const emergencyCommittee = new PublicKey(configInfo.data.subarray(72, 104));
  if (!emergencyCommittee.equals(wallet.publicKey)) {
    throw new Error(
      `wallet ${wallet.publicKey.toBase58()} is not the emergency committee ${emergencyCommittee.toBase58()}`
    );
  }

  const [escrowPda] = deriveEscrowPda(forwarderProgramId, tokenMint);
  const escrowAta = await getAssociatedTokenAddress(tokenMint, escrowPda, true);
  const escrowInfo = await connection.getAccountInfo(escrowAta);
  if (!escrowInfo) {
    console.log(
      `Escrow ATA ${escrowAta.toBase58()} does not exist; nothing to recover.`
    );
    return;
  }

  const balance = await connection.getTokenAccountBalance(escrowAta);
  const rawAmount = BigInt(balance.value.amount);
  if (rawAmount === 0n) {
    console.log(
      `Escrow ATA ${escrowAta.toBase58()} has zero balance; leaving it open.`
    );
    return;
  }

  const recipientAtaAccount = await getOrCreateAssociatedTokenAccount(
    connection,
    wallet.payer,
    tokenMint,
    recipientOwner
  );

  console.log("Forwarder:      ", forwarderProgramId.toBase58());
  console.log("Emergency wallet:", wallet.publicKey.toBase58());
  console.log("Token mint:     ", tokenMint.toBase58());
  console.log("Escrow PDA:     ", escrowPda.toBase58());
  console.log("Escrow ATA:     ", escrowAta.toBase58());
  console.log("Recipient owner:", recipientOwner.toBase58());
  console.log("Recipient ATA:  ", recipientAtaAccount.address.toBase58());
  console.log("Amount:         ", balance.value.uiAmountString);

  const signature = await program.methods
    .closeEscrow()
    .accounts({
      authority: wallet.publicKey,
      config: configPda,
      escrowAta,
      escrowPda,
      recipientAta: recipientAtaAccount.address,
      tokenMint,
      tokenProgram: TOKEN_PROGRAM_ID,
    })
    .rpc();

  console.log("Escrow recovery transaction:", signature);
}

main().catch((err) => {
  console.error("Escrow recovery failed:", err.message || err);
  process.exit(1);
});
