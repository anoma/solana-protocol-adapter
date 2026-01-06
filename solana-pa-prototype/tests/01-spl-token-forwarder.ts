import * as anchor from "@coral-xyz/anchor";
import { Program } from "@coral-xyz/anchor";
import {
  PublicKey,
  SystemProgram,
  Keypair,
  LAMPORTS_PER_SOL,
  Ed25519Program,
  Transaction,
} from "@solana/web3.js";
import {
  createMint,
  createAccount,
  getOrCreateAssociatedTokenAccount,
  mintTo,
  approve,
  getAccount,
  TOKEN_PROGRAM_ID,
} from "@solana/spl-token";
import { assert } from "chai";
import { createHash } from "crypto";
import * as nacl from "tweetnacl";
import { SplTokenForwarder } from "../target/types/spl_token_forwarder";

// Constants
const SPL_TOKEN_FORWARDER_PROGRAM_ID = new PublicKey("6cMwWUEoTnj8ManPCwAtXw5vdnp16mQKfUTdbxLNszN1");
const OP_WRAP = 0;
const OP_UNWRAP = 1;

// Helper to derive PDAs
function deriveConfigPda(programId: PublicKey): [PublicKey, number] {
  return PublicKey.findProgramAddressSync([Buffer.from("config")], programId);
}

function deriveEscrowPda(programId: PublicKey, tokenMint: PublicKey): [PublicKey, number] {
  return PublicKey.findProgramAddressSync(
    [Buffer.from("escrow"), tokenMint.toBuffer()],
    programId
  );
}

function deriveNoncePda(programId: PublicKey, user: PublicKey, nonce: bigint): [PublicKey, number] {
  const nonceBuffer = Buffer.alloc(8);
  nonceBuffer.writeBigUInt64LE(nonce);
  return PublicKey.findProgramAddressSync(
    [Buffer.from("nonce"), user.toBuffer(), nonceBuffer],
    programId
  );
}

// Helper to create wrap message hash
function createWrapMessageHash(
  tokenMint: PublicKey,
  amount: bigint,
  nonce: bigint,
  deadline: bigint,
  actionTreeRoot: Buffer
): Buffer {
  // Layout: token_mint(32) + amount(8) + nonce(8) + deadline(8) + action_tree_root(32) = 88 bytes
  const message = Buffer.alloc(88);
  tokenMint.toBuffer().copy(message, 0);
  message.writeBigUInt64LE(amount, 32);
  message.writeBigUInt64LE(nonce, 40);
  message.writeBigInt64LE(deadline, 48);
  actionTreeRoot.copy(message, 56);

  // SHA-256 hash
  return createHash("sha256").update(message).digest();
}

// Helper to encode wrap input
function encodeWrapInput(
  tokenMint: PublicKey,
  amount: bigint,
  user: PublicKey,
  nonce: bigint,
  deadline: bigint,
  actionTreeRoot: Buffer,
  signature: Buffer,
  ed25519IxIndex: number
): Buffer {
  // Layout: op(1) + token_mint(32) + amount(8) + user(32) + nonce(8) + deadline(8) + action_tree_root(32) + signature(64) + ed25519_ix_index(1) = 186 bytes
  const input = Buffer.alloc(186);
  let offset = 0;

  input.writeUInt8(OP_WRAP, offset);
  offset += 1;

  tokenMint.toBuffer().copy(input, offset);
  offset += 32;

  input.writeBigUInt64LE(amount, offset);
  offset += 8;

  user.toBuffer().copy(input, offset);
  offset += 32;

  input.writeBigUInt64LE(nonce, offset);
  offset += 8;

  input.writeBigInt64LE(deadline, offset);
  offset += 8;

  actionTreeRoot.copy(input, offset);
  offset += 32;

  signature.copy(input, offset);
  offset += 64;

  input.writeUInt8(ed25519IxIndex, offset);

  return input;
}

// Helper to encode unwrap input
function encodeUnwrapInput(tokenMint: PublicKey, amount: bigint, recipient: PublicKey): Buffer {
  // Layout: op(1) + token_mint(32) + amount(8) + recipient(32) = 73 bytes
  const input = Buffer.alloc(73);
  let offset = 0;

  input.writeUInt8(OP_UNWRAP, offset);
  offset += 1;

  tokenMint.toBuffer().copy(input, offset);
  offset += 32;

  input.writeBigUInt64LE(amount, offset);
  offset += 8;

  recipient.toBuffer().copy(input, offset);

  return input;
}

async function airdrop(provider: anchor.AnchorProvider, to: PublicKey, sol: number) {
  const sig = await provider.connection.requestAirdrop(to, sol * LAMPORTS_PER_SOL);
  await provider.connection.confirmTransaction(sig, "confirmed");
}

describe("spl-token-forwarder", () => {
  const provider = anchor.AnchorProvider.env();
  anchor.setProvider(provider);

  // We'll use the program from the workspace
  let program: Program<SplTokenForwarder>;

  // Test accounts
  let authority: Keypair;
  let protocolAdapter: Keypair;
  let emergencyCommittee: Keypair;
  let emergencyCaller: Keypair;
  let user: Keypair;
  let recipient: Keypair;

  // Token accounts
  let tokenMint: PublicKey;
  let userAta: PublicKey;
  let escrowAta: PublicKey;
  let recipientAta: PublicKey;

  // PDAs
  let configPda: PublicKey;
  let configBump: number;
  let escrowPda: PublicKey;
  let escrowBump: number;

  // Logic ref for this forwarder
  let logicRef: Buffer;

  before(async () => {
    // Load program
    try {
      program = anchor.workspace.SplTokenForwarder as Program<SplTokenForwarder>;
    } catch (e) {
      console.log("SPL Token Forwarder program not found in workspace, skipping tests");
      return;
    }

    // Generate keypairs
    authority = Keypair.generate();
    protocolAdapter = Keypair.generate();
    emergencyCommittee = Keypair.generate();
    emergencyCaller = Keypair.generate();
    user = Keypair.generate();
    recipient = Keypair.generate();

    // Airdrop SOL
    await airdrop(provider, authority.publicKey, 10);
    await airdrop(provider, user.publicKey, 10);
    await airdrop(provider, recipient.publicKey, 1);
    await airdrop(provider, emergencyCommittee.publicKey, 1);
    await airdrop(provider, emergencyCaller.publicKey, 1);

    // Derive PDAs
    [configPda, configBump] = deriveConfigPda(program.programId);

    // Set logic_ref for standalone tests - must match fixture-gen's value
    // fixture-gen uses: SHA256("spl_token_forwarder_test_logic_ref")
    logicRef = createHash("sha256").update("spl_token_forwarder_test_logic_ref").digest();

    // Create token mint
    tokenMint = await createMint(
      provider.connection,
      authority,
      authority.publicKey,
      null,
      6 // 6 decimals like USDC
    );

    // Derive escrow PDA
    [escrowPda, escrowBump] = deriveEscrowPda(program.programId, tokenMint);

    // Create token accounts
    userAta = await createAccount(provider.connection, user, tokenMint, user.publicKey);

    // Create escrow ATA owned by escrow PDA (allowOwnerOffCurve for PDA owner)
    const escrowAtaInfo = await getOrCreateAssociatedTokenAccount(
      provider.connection,
      authority,
      tokenMint,
      escrowPda,
      true // allowOwnerOffCurve - required for PDA owners
    );
    escrowAta = escrowAtaInfo.address;

    recipientAta = await createAccount(
      provider.connection,
      recipient,
      tokenMint,
      recipient.publicKey
    );

    // Mint tokens to user
    await mintTo(provider.connection, authority, tokenMint, userAta, authority, 1000_000_000); // 1000 tokens
  });

  describe("initialize", () => {
    it("initializes the forwarder config", async () => {
      if (!program) return;

      await program.methods
        .initialize(protocolAdapter.publicKey, Array.from(logicRef), emergencyCommittee.publicKey)
        .accounts({
          authority: authority.publicKey,
          // config: auto-derived from PDA seeds
          // systemProgram: auto-filled (fixed address)
        })
        .signers([authority])
        .rpc();

      // Fetch and verify config
      const config = await program.account.config.fetch(configPda);
      assert.ok(config.protocolAdapter.equals(protocolAdapter.publicKey));
      assert.deepEqual(config.logicRef, Array.from(logicRef));
      assert.ok(config.emergencyCommittee.equals(emergencyCommittee.publicKey));
      assert.ok(config.emergencyCaller.equals(PublicKey.default));
      assert.equal(config.isStopped, false);
    });
  });

  describe("wrap", () => {
    it("wraps tokens with valid Ed25519 signature", async () => {
      if (!program) return;

      const amount = BigInt(100_000_000); // 100 tokens
      const nonce = BigInt(1);
      const deadline = BigInt(Math.floor(Date.now() / 1000) + 3600); // 1 hour from now
      const actionTreeRoot = Buffer.alloc(32);
      actionTreeRoot.fill(0xaa);

      // Create message hash
      const messageHash = createWrapMessageHash(tokenMint, amount, nonce, deadline, actionTreeRoot);

      // Sign with user's Ed25519 keypair
      const signature = nacl.sign.detached(messageHash, user.secretKey);

      // Approve escrow PDA as delegate
      await approve(provider.connection, user, userAta, escrowPda, user, Number(amount));

      // Derive nonce PDA
      const [noncePda] = deriveNoncePda(program.programId, user.publicKey, nonce);

      // Create Ed25519 verify instruction (must be first in transaction)
      const ed25519Ix = Ed25519Program.createInstructionWithPublicKey({
        publicKey: user.publicKey.toBytes(),
        message: messageHash,
        signature: Buffer.from(signature),
      });

      // Create wrap input
      const wrapInput = encodeWrapInput(
        tokenMint,
        amount,
        user.publicKey,
        nonce,
        deadline,
        actionTreeRoot,
        Buffer.from(signature),
        0 // Ed25519 instruction is at index 0
      );

      // Build transaction with Ed25519 verify first
      const tx = new Transaction();
      tx.add(ed25519Ix);

      // Add forward_call instruction
      const forwardCallIx = await program.methods
        .forwardCall(Array.from(logicRef), wrapInput)
        .accounts({
          caller: protocolAdapter.publicKey, // Must match config.protocol_adapter
        })
        .remainingAccounts([
          { pubkey: userAta, isSigner: false, isWritable: true },
          { pubkey: escrowAta, isSigner: false, isWritable: true },
          { pubkey: escrowPda, isSigner: false, isWritable: false },
          { pubkey: noncePda, isSigner: false, isWritable: true },
          { pubkey: TOKEN_PROGRAM_ID, isSigner: false, isWritable: false },
          { pubkey: SystemProgram.programId, isSigner: false, isWritable: false },
          { pubkey: user.publicKey, isSigner: true, isWritable: true }, // payer for nonce PDA
          { pubkey: tokenMint, isSigner: false, isWritable: false },
        ])
        .instruction();

      tx.add(forwardCallIx);

      // Send transaction
      await provider.sendAndConfirm(tx, [user]);

      // Verify escrow received tokens
      const escrowAccount = await getAccount(provider.connection, escrowAta);
      assert.equal(escrowAccount.amount.toString(), amount.toString());

      // Verify user balance decreased
      const userAccount = await getAccount(provider.connection, userAta);
      assert.equal(userAccount.amount.toString(), (1000_000_000n - amount).toString());
    });

    it("rejects wrap with expired deadline", async () => {
      if (!program) return;

      const amount = BigInt(50_000_000);
      const nonce = BigInt(2);
      const deadline = BigInt(Math.floor(Date.now() / 1000) - 3600); // 1 hour ago (expired)
      const actionTreeRoot = Buffer.alloc(32);

      const messageHash = createWrapMessageHash(tokenMint, amount, nonce, deadline, actionTreeRoot);
      const signature = nacl.sign.detached(messageHash, user.secretKey);

      await approve(provider.connection, user, userAta, escrowPda, user, Number(amount));

      const [noncePda] = deriveNoncePda(program.programId, user.publicKey, nonce);

      const ed25519Ix = Ed25519Program.createInstructionWithPublicKey({
        publicKey: user.publicKey.toBytes(),
        message: messageHash,
        signature: Buffer.from(signature),
      });

      const wrapInput = encodeWrapInput(
        tokenMint,
        amount,
        user.publicKey,
        nonce,
        deadline,
        actionTreeRoot,
        Buffer.from(signature),
        0
      );

      const tx = new Transaction();
      tx.add(ed25519Ix);

      const forwardCallIx = await program.methods
        .forwardCall(Array.from(logicRef), wrapInput)
        .accounts({
          caller: protocolAdapter.publicKey,
        })
        .remainingAccounts([
          { pubkey: userAta, isSigner: false, isWritable: true },
          { pubkey: escrowAta, isSigner: false, isWritable: true },
          { pubkey: escrowPda, isSigner: false, isWritable: false },
          { pubkey: noncePda, isSigner: false, isWritable: true },
          { pubkey: TOKEN_PROGRAM_ID, isSigner: false, isWritable: false },
          { pubkey: SystemProgram.programId, isSigner: false, isWritable: false },
          { pubkey: user.publicKey, isSigner: true, isWritable: true },
          { pubkey: tokenMint, isSigner: false, isWritable: false },
        ])
        .instruction();

      tx.add(forwardCallIx);

      try {
        await provider.sendAndConfirm(tx, [user]);
        assert.fail("Expected transaction to fail");
      } catch (e: any) {
        assert.include(e.toString(), "DeadlineExpired");
      }
    });

    it("rejects wrap with already used nonce", async () => {
      if (!program) return;

      const amount = BigInt(50_000_000);
      const nonce = BigInt(1); // Same nonce as first wrap
      const deadline = BigInt(Math.floor(Date.now() / 1000) + 3600);
      const actionTreeRoot = Buffer.alloc(32);

      const messageHash = createWrapMessageHash(tokenMint, amount, nonce, deadline, actionTreeRoot);
      const signature = nacl.sign.detached(messageHash, user.secretKey);

      await approve(provider.connection, user, userAta, escrowPda, user, Number(amount));

      const [noncePda] = deriveNoncePda(program.programId, user.publicKey, nonce);

      const ed25519Ix = Ed25519Program.createInstructionWithPublicKey({
        publicKey: user.publicKey.toBytes(),
        message: messageHash,
        signature: Buffer.from(signature),
      });

      const wrapInput = encodeWrapInput(
        tokenMint,
        amount,
        user.publicKey,
        nonce,
        deadline,
        actionTreeRoot,
        Buffer.from(signature),
        0
      );

      const tx = new Transaction();
      tx.add(ed25519Ix);

      const forwardCallIx = await program.methods
        .forwardCall(Array.from(logicRef), wrapInput)
        .accounts({
          caller: protocolAdapter.publicKey,
        })
        .remainingAccounts([
          { pubkey: userAta, isSigner: false, isWritable: true },
          { pubkey: escrowAta, isSigner: false, isWritable: true },
          { pubkey: escrowPda, isSigner: false, isWritable: false },
          { pubkey: noncePda, isSigner: false, isWritable: true },
          { pubkey: TOKEN_PROGRAM_ID, isSigner: false, isWritable: false },
          { pubkey: SystemProgram.programId, isSigner: false, isWritable: false },
          { pubkey: user.publicKey, isSigner: true, isWritable: true },
          { pubkey: tokenMint, isSigner: false, isWritable: false },
        ])
        .instruction();

      tx.add(forwardCallIx);

      try {
        await provider.sendAndConfirm(tx, [user]);
        assert.fail("Expected transaction to fail");
      } catch (e: any) {
        assert.include(e.toString(), "NonceAlreadyUsed");
      }
    });
  });

  describe("unwrap", () => {
    it("unwraps tokens to recipient", async () => {
      if (!program) return;

      const amount = BigInt(50_000_000); // 50 tokens

      // Create unwrap input
      const unwrapInput = encodeUnwrapInput(tokenMint, amount, recipient.publicKey);

      // Get balances before
      const escrowBefore = await getAccount(provider.connection, escrowAta);
      const recipientBefore = await getAccount(provider.connection, recipientAta);

      // Call forward_call with unwrap
      await program.methods
        .forwardCall(Array.from(logicRef), unwrapInput)
        .accounts({
          caller: protocolAdapter.publicKey,
        })
        .remainingAccounts([
          { pubkey: escrowAta, isSigner: false, isWritable: true },
          { pubkey: recipientAta, isSigner: false, isWritable: true },
          { pubkey: escrowPda, isSigner: false, isWritable: false },
          { pubkey: TOKEN_PROGRAM_ID, isSigner: false, isWritable: false },
          { pubkey: tokenMint, isSigner: false, isWritable: false },
        ])
        .rpc();

      // Verify balances
      const escrowAfter = await getAccount(provider.connection, escrowAta);
      const recipientAfter = await getAccount(provider.connection, recipientAta);

      assert.equal(
        escrowAfter.amount.toString(),
        (BigInt(escrowBefore.amount.toString()) - amount).toString()
      );
      assert.equal(
        recipientAfter.amount.toString(),
        (BigInt(recipientBefore.amount.toString()) + amount).toString()
      );
    });
  });

  describe("emergency operations", () => {
    it("allows emergency committee to stop forwarder", async () => {
      if (!program) return;

      // First verify not stopped
      let config = await program.account.config.fetch(configPda);
      assert.equal(config.isStopped, false);

      // Emergency stop
      await program.methods
        .emergencyStop()
        .accounts({
          committee: emergencyCommittee.publicKey,
        })
        .signers([emergencyCommittee])
        .rpc();

      // Verify stopped
      config = await program.account.config.fetch(configPda);
      assert.equal(config.isStopped, true);
    });

    it("allows emergency committee to set emergency caller (when stopped)", async () => {
      if (!program) return;

      await program.methods
        .setEmergencyCaller(emergencyCaller.publicKey)
        .accounts({
          committee: emergencyCommittee.publicKey,
        })
        .signers([emergencyCommittee])
        .rpc();

      const config = await program.account.config.fetch(configPda);
      assert.ok(config.emergencyCaller.equals(emergencyCaller.publicKey));
    });

    it("rejects setting emergency caller twice", async () => {
      if (!program) return;

      const anotherCaller = Keypair.generate();

      try {
        await program.methods
          .setEmergencyCaller(anotherCaller.publicKey)
          .accounts({
            committee: emergencyCommittee.publicKey,
            // config: auto-derived from PDA seeds
          })
          .signers([emergencyCommittee])
          .rpc();
        assert.fail("Expected transaction to fail");
      } catch (e: any) {
        assert.include(e.toString(), "EmergencyCallerAlreadySet");
      }
    });

    it("allows emergency caller to withdraw", async () => {
      if (!program) return;

      const amount = BigInt(25_000_000); // 25 tokens

      // Encode emergency withdraw: op(1) + token_mint(32) + amount(8) + recipient(32) = 73 bytes
      const input = Buffer.alloc(73);
      input.writeUInt8(0, 0); // OP_EMERGENCY_WITHDRAW
      tokenMint.toBuffer().copy(input, 1);
      input.writeBigUInt64LE(amount, 33);
      recipient.publicKey.toBuffer().copy(input, 41);

      const escrowBefore = await getAccount(provider.connection, escrowAta);
      const recipientBefore = await getAccount(provider.connection, recipientAta);

      await program.methods
        .forwardEmergencyCall(input)
        .accounts({
          caller: emergencyCaller.publicKey,
          // config: auto-derived from PDA seeds
        })
        .remainingAccounts([
          { pubkey: escrowAta, isSigner: false, isWritable: true },
          { pubkey: recipientAta, isSigner: false, isWritable: true },
          { pubkey: escrowPda, isSigner: false, isWritable: false },
          { pubkey: TOKEN_PROGRAM_ID, isSigner: false, isWritable: false },
        ])
        .signers([emergencyCaller])
        .rpc();

      const escrowAfter = await getAccount(provider.connection, escrowAta);
      const recipientAfter = await getAccount(provider.connection, recipientAta);

      assert.equal(
        escrowAfter.amount.toString(),
        (BigInt(escrowBefore.amount.toString()) - amount).toString()
      );
      assert.equal(
        recipientAfter.amount.toString(),
        (BigInt(recipientBefore.amount.toString()) + amount).toString()
      );
    });
  });

  describe("error conditions", () => {
    it("rejects forward_call with wrong logic_ref", async () => {
      if (!program) return;

      const wrongLogicRef = Buffer.alloc(32);
      wrongLogicRef.fill(0x99);

      const unwrapInput = encodeUnwrapInput(tokenMint, BigInt(1000), recipient.publicKey);

      try {
        await program.methods
          .forwardCall(Array.from(wrongLogicRef), unwrapInput)
          .accounts({
            caller: protocolAdapter.publicKey,
          })
          .remainingAccounts([
            { pubkey: escrowAta, isSigner: false, isWritable: true },
            { pubkey: recipientAta, isSigner: false, isWritable: true },
            { pubkey: escrowPda, isSigner: false, isWritable: false },
            { pubkey: TOKEN_PROGRAM_ID, isSigner: false, isWritable: false },
            { pubkey: tokenMint, isSigner: false, isWritable: false },
          ])
          .rpc();
        assert.fail("Expected transaction to fail");
      } catch (e: any) {
        assert.include(e.toString(), "UnauthorizedLogicRef");
      }
    });
  });
});
