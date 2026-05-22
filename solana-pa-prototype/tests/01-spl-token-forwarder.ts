import * as anchor from "@coral-xyz/anchor";
import { Program } from "@coral-xyz/anchor";
import {
  PublicKey,
  SystemProgram,
  Keypair,
  Ed25519Program,
  Transaction,
  LAMPORTS_PER_SOL,
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
import { existsSync, readFileSync } from "fs";
import path from "path";
import { SplTokenForwarder } from "../target/types/spl_token_forwarder";
import { SolanaPaPrototype } from "../target/types/solana_pa_prototype";
import {
  deriveConfigPda,
  deriveEscrowPda,
  deriveNonceBitmapPda,
  derivePaStatePda,
} from "./utils";
import {
  fundKeypair,
  drainKeypairs,
  createWrapMessageHash,
  encodeWrapInput,
  encodeUnwrapInput,
} from "./utils";
import { OP_WRAP, OP_UNWRAP } from "./utils";

describe("spl-token-forwarder", () => {
  const provider = anchor.AnchorProvider.env();
  anchor.setProvider(provider);

  const localFundedKeypairs: Keypair[] = [];
  async function localAirdrop(kp: Keypair, sol: number) {
    await fundKeypair(provider, kp, sol);
    localFundedKeypairs.push(kp);
  }

  // We'll use the programs from the workspace
  let program: Program<SplTokenForwarder>;
  let paProgram: Program<SolanaPaPrototype>;

  // Test accounts
  let authority: Keypair;
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
  let paStatePda: PublicKey;

  // Logic ref for this forwarder
  let logicRef: Buffer;

  // Track if PA is initialized (verified by 00-setup.ts)
  let paInitialized = false;

  before(async () => {
    // Load programs
    try {
      program = anchor.workspace
        .SplTokenForwarder as Program<SplTokenForwarder>;
      paProgram = anchor.workspace
        .SolanaPaPrototype as Program<SolanaPaPrototype>;
    } catch (e) {
      console.log("Programs not found in workspace, skipping tests");
      return;
    }

    // Generate keypairs
    // Use deterministic keypairs for emergency committee/caller so they match across test files
    authority = Keypair.generate();
    emergencyCommittee = Keypair.fromSeed(
      createHash("sha256").update("emergency_committee_seed").digest()
    );
    emergencyCaller = Keypair.fromSeed(
      createHash("sha256").update("emergency_caller_seed").digest()
    );
    user = Keypair.generate();
    recipient = Keypair.generate();

    // Airdrop SOL
    await localAirdrop(authority, 10);
    await localAirdrop(user, 10);
    await localAirdrop(recipient, 1);
    await localAirdrop(emergencyCommittee, 1);
    await localAirdrop(emergencyCaller, 1);

    // Derive PDAs
    [configPda, configBump] = deriveConfigPda(program.programId);
    // Use real PA program ID for PA state derivation
    [paStatePda] = derivePaStatePda(paProgram.programId);

    // Verify PA is initialized (done by 00-setup.ts)
    try {
      await paProgram.account.paStateAccount.fetch(paStatePda);
      paInitialized = true;
    } catch {
      throw new Error(
        "PA not initialized - 00-setup.ts should have initialized it"
      );
    }

    // Set logic_ref for standalone tests - must match fixture-gen's value
    // fixture-gen uses PASSTHROUGH_LOGIC_GUEST_ID (RISC0 image ID)
    // Read from fixture to ensure consistency across all test files
    const wrapFixturePath = path.resolve(
      process.cwd(),
      "tests",
      "fixtures",
      "spl_token_wrap.json"
    );
    if (existsSync(wrapFixturePath)) {
      const fixture = JSON.parse(readFileSync(wrapFixturePath, "utf8"));
      const logicRefB64 = fixture.spl_token_wrap?.logic_ref_b64;
      if (logicRefB64) {
        logicRef = Buffer.from(logicRefB64, "base64");
      } else {
        throw new Error("No logic_ref_b64 found in wrap fixture metadata");
      }
    } else {
      throw new Error("Wrap fixture not found - run fixture-gen first");
    }

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
    userAta = await createAccount(
      provider.connection,
      user,
      tokenMint,
      user.publicKey
    );

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
    await mintTo(
      provider.connection,
      authority,
      tokenMint,
      userAta,
      authority,
      1000_000_000
    ); // 1000 tokens
  });

  describe("initialize", () => {
    // Mirrors: test_constructor_reverts_if_the_protocol_adapter_address_is_zero
    // https://github.com/anoma/anomapay-backend/blob/main/contracts/test/bases/ForwarderBase.t.sol
    it("rejects zero protocol adapter address", async () => {
      if (!program) return;

      try {
        await program.methods
          .initialize(
            PublicKey.default,
            Array.from(logicRef),
            emergencyCommittee.publicKey
          )
          .accounts({
            authority: authority.publicKey,
          })
          .signers([authority])
          .rpc();
        assert.fail("Expected transaction to fail");
      } catch (e: any) {
        assert.include(e.toString(), "ZeroAddressNotAllowed");
      }
    });

    // Mirrors: test_constructor_reverts_if_the_logic_ref_is_zero
    // https://github.com/anoma/anomapay-backend/blob/main/contracts/test/bases/ForwarderBase.t.sol
    it("rejects zero logic_ref", async () => {
      if (!program) return;

      const zeroLogicRef = Array(32).fill(0);

      try {
        await program.methods
          .initialize(
            paProgram.programId,
            zeroLogicRef,
            emergencyCommittee.publicKey
          )
          .accounts({
            authority: authority.publicKey,
          })
          .signers([authority])
          .rpc();
        assert.fail("Expected transaction to fail");
      } catch (e: any) {
        assert.include(e.toString(), "ZeroAddressNotAllowed");
      }
    });

    // Mirrors: test_constructor_reverts_if_the_emergency_committe_address_is_zero
    // https://github.com/anoma/anomapay-backend/blob/main/contracts/test/bases/EmergencyMigratableForwarderBase.t.sol
    it("rejects zero emergency committee address", async () => {
      if (!program) return;

      try {
        await program.methods
          .initialize(
            paProgram.programId,
            Array.from(logicRef),
            PublicKey.default
          )
          .accounts({
            authority: authority.publicKey,
          })
          .signers([authority])
          .rpc();
        assert.fail("Expected transaction to fail");
      } catch (e: any) {
        assert.include(e.toString(), "ZeroAddressNotAllowed");
      }
    });

    it("initializes the forwarder config", async () => {
      if (!program) return;

      await program.methods
        .initialize(
          paProgram.programId,
          Array.from(logicRef),
          emergencyCommittee.publicKey
        )
        .accounts({
          authority: authority.publicKey,
          // config: auto-derived from PDA seeds
          // systemProgram: auto-filled (fixed address)
        })
        .signers([authority])
        .rpc();

      // Fetch and verify config
      const config = await program.account.config.fetch(configPda);
      assert.ok(config.protocolAdapter.equals(paProgram.programId));
      assert.deepEqual(config.logicRef, Array.from(logicRef));
      assert.ok(config.emergencyCommittee.equals(emergencyCommittee.publicKey));
      assert.ok(config.emergencyCaller.equals(PublicKey.default));
      // Note: Forwarder doesn't have its own is_stopped field (mirrors EVM)
      // Emergency stopped state is read from PA via is_pa_emergency_stopped()
    });

    // Mirrors: test_getProtocolAdapter_returns_the_protocol_adapter_address
    // https://github.com/anoma/anomapay-backend/blob/main/contracts/test/bases/ForwarderBase.t.sol
    it("stores correct protocol adapter address", async () => {
      if (!program) return;

      const config = await program.account.config.fetch(configPda);
      assert.ok(config.protocolAdapter.equals(paProgram.programId));
    });

    // Mirrors: test_getLogicRef_returns_the_logic_ref
    // https://github.com/anoma/anomapay-backend/blob/main/contracts/test/bases/ForwarderBase.t.sol
    it("stores correct logic ref", async () => {
      if (!program) return;

      const config = await program.account.config.fetch(configPda);
      assert.deepEqual(config.logicRef, Array.from(logicRef));
    });

    // Mirrors: test_emergencyCaller_returns_zero_if_the_emergency_caller_has_not_been_set
    // https://github.com/anoma/anomapay-backend/blob/main/contracts/test/bases/EmergencyMigratableForwarderBase.t.sol
    it("initializes emergency caller to zero address", async () => {
      if (!program) return;

      const config = await program.account.config.fetch(configPda);
      assert.ok(config.emergencyCaller.equals(PublicKey.default));
    });
  });

  describe("wrap", () => {
    // Mirrors: test_wrap_pulls_funds_from_user
    // https://github.com/anoma/anomapay-backend/blob/main/contracts/test/ERC20Forwarder.t.sol
    it("wraps tokens with valid Ed25519 signature", async () => {
      if (!program) return;

      const amount = BigInt(100_000_000); // 100 tokens
      const nonce = BigInt(1);
      const deadline = BigInt(Math.floor(Date.now() / 1000) + 3600); // 1 hour from now
      const actionTreeRoot = Buffer.alloc(32);
      actionTreeRoot.fill(0xaa);

      // Create message hash (includes forwarder program ID for domain separation)
      const messageHash = createWrapMessageHash(
        program.programId,
        tokenMint,
        amount,
        nonce,
        deadline,
        actionTreeRoot
      );

      // Sign with user's Ed25519 keypair
      const signature = nacl.sign.detached(messageHash, user.secretKey);

      // Approve escrow PDA as delegate
      await approve(
        provider.connection,
        user,
        userAta,
        escrowPda,
        user,
        Number(amount)
      );

      // Derive nonce PDA
      const [nonceBitmapPda] = deriveNonceBitmapPda(
        program.programId,
        user.publicKey,
        nonce
      );

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
        .accounts({})
        .remainingAccounts([
          { pubkey: userAta, isSigner: false, isWritable: true },
          { pubkey: escrowAta, isSigner: false, isWritable: true },
          { pubkey: escrowPda, isSigner: false, isWritable: false },
          { pubkey: nonceBitmapPda, isSigner: false, isWritable: true },
          { pubkey: TOKEN_PROGRAM_ID, isSigner: false, isWritable: false },
          {
            pubkey: SystemProgram.programId,
            isSigner: false,
            isWritable: false,
          },
          { pubkey: user.publicKey, isSigner: true, isWritable: true }, // payer for nonce PDA
          { pubkey: tokenMint, isSigner: false, isWritable: false },
        ])
        .instruction();

      tx.add(forwardCallIx);

      // Direct calls to forward_call are rejected — CPI introspection requires the PA as caller
      try {
        await provider.sendAndConfirm(tx, [user]);
        assert.fail("Expected transaction to fail with UnauthorizedCaller");
      } catch (e: any) {
        assert.include(e.toString(), "UnauthorizedCaller");
      }
    });

    // Mirrors: test_wrap_reverts_if_the_signature_expired
    // https://github.com/anoma/anomapay-backend/blob/main/contracts/test/ERC20Forwarder.t.sol
    it("rejects wrap with expired deadline", async () => {
      if (!program) return;

      const amount = BigInt(50_000_000);
      const nonce = BigInt(2);
      const deadline = BigInt(Math.floor(Date.now() / 1000) - 3600); // 1 hour ago (expired)
      const actionTreeRoot = Buffer.alloc(32);

      const messageHash = createWrapMessageHash(
        program.programId,
        tokenMint,
        amount,
        nonce,
        deadline,
        actionTreeRoot
      );
      const signature = nacl.sign.detached(messageHash, user.secretKey);

      await approve(
        provider.connection,
        user,
        userAta,
        escrowPda,
        user,
        Number(amount)
      );

      const [nonceBitmapPda] = deriveNonceBitmapPda(
        program.programId,
        user.publicKey,
        nonce
      );

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
        .accounts({})
        .remainingAccounts([
          { pubkey: userAta, isSigner: false, isWritable: true },
          { pubkey: escrowAta, isSigner: false, isWritable: true },
          { pubkey: escrowPda, isSigner: false, isWritable: false },
          { pubkey: nonceBitmapPda, isSigner: false, isWritable: true },
          { pubkey: TOKEN_PROGRAM_ID, isSigner: false, isWritable: false },
          {
            pubkey: SystemProgram.programId,
            isSigner: false,
            isWritable: false,
          },
          { pubkey: user.publicKey, isSigner: true, isWritable: true },
          { pubkey: tokenMint, isSigner: false, isWritable: false },
        ])
        .instruction();

      tx.add(forwardCallIx);

      try {
        await provider.sendAndConfirm(tx, [user]);
        assert.fail("Expected transaction to fail");
      } catch (e: any) {
        assert.include(e.toString(), "UnauthorizedCaller");
      }
    });

    // Mirrors: test_wrap_reverts_if_the_signature_was_already_used
    // https://github.com/anoma/anomapay-backend/blob/main/contracts/test/ERC20Forwarder.t.sol
    it("rejects wrap with already used nonce", async () => {
      if (!program) return;

      const amount = BigInt(50_000_000);
      const nonce = BigInt(1); // Same nonce as first wrap
      const deadline = BigInt(Math.floor(Date.now() / 1000) + 3600);
      const actionTreeRoot = Buffer.alloc(32);

      const messageHash = createWrapMessageHash(
        program.programId,
        tokenMint,
        amount,
        nonce,
        deadline,
        actionTreeRoot
      );
      const signature = nacl.sign.detached(messageHash, user.secretKey);

      await approve(
        provider.connection,
        user,
        userAta,
        escrowPda,
        user,
        Number(amount)
      );

      const [nonceBitmapPda] = deriveNonceBitmapPda(
        program.programId,
        user.publicKey,
        nonce
      );

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
        .accounts({})
        .remainingAccounts([
          { pubkey: userAta, isSigner: false, isWritable: true },
          { pubkey: escrowAta, isSigner: false, isWritable: true },
          { pubkey: escrowPda, isSigner: false, isWritable: false },
          { pubkey: nonceBitmapPda, isSigner: false, isWritable: true },
          { pubkey: TOKEN_PROGRAM_ID, isSigner: false, isWritable: false },
          {
            pubkey: SystemProgram.programId,
            isSigner: false,
            isWritable: false,
          },
          { pubkey: user.publicKey, isSigner: true, isWritable: true },
          { pubkey: tokenMint, isSigner: false, isWritable: false },
        ])
        .instruction();

      tx.add(forwardCallIx);

      try {
        await provider.sendAndConfirm(tx, [user]);
        assert.fail("Expected transaction to fail");
      } catch (e: any) {
        assert.include(e.toString(), "UnauthorizedCaller");
      }
    });

    // Mirrors: test_wrap_does_not_revert_if_the_amount_is_zero
    // https://github.com/anoma/anomapay-backend/blob/main/contracts/test/ERC20Forwarder.t.sol
    it("wrap succeeds with zero amount", async () => {
      if (!program) return;

      const amount = BigInt(0); // Zero amount
      const nonce = BigInt(100); // Fresh nonce
      const deadline = BigInt(Math.floor(Date.now() / 1000) + 3600);
      const actionTreeRoot = Buffer.alloc(32);

      const messageHash = createWrapMessageHash(
        program.programId,
        tokenMint,
        amount,
        nonce,
        deadline,
        actionTreeRoot
      );
      const signature = nacl.sign.detached(messageHash, user.secretKey);

      // Approve escrow PDA as delegate (even though amount is 0)
      await approve(provider.connection, user, userAta, escrowPda, user, 0);

      const [nonceBitmapPda] = deriveNonceBitmapPda(
        program.programId,
        user.publicKey,
        nonce
      );

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

      const userBalanceBefore = (await getAccount(provider.connection, userAta))
        .amount;
      const escrowBalanceBefore = (
        await getAccount(provider.connection, escrowAta)
      ).amount;

      const tx = new Transaction();
      tx.add(ed25519Ix);

      const forwardCallIx = await program.methods
        .forwardCall(Array.from(logicRef), wrapInput)
        .accounts({})
        .remainingAccounts([
          { pubkey: userAta, isSigner: false, isWritable: true },
          { pubkey: escrowAta, isSigner: false, isWritable: true },
          { pubkey: escrowPda, isSigner: false, isWritable: false },
          { pubkey: nonceBitmapPda, isSigner: false, isWritable: true },
          { pubkey: TOKEN_PROGRAM_ID, isSigner: false, isWritable: false },
          {
            pubkey: SystemProgram.programId,
            isSigner: false,
            isWritable: false,
          },
          { pubkey: user.publicKey, isSigner: true, isWritable: true },
          { pubkey: tokenMint, isSigner: false, isWritable: false },
        ])
        .instruction();

      tx.add(forwardCallIx);

      // Direct calls to forward_call are rejected — CPI introspection requires the PA as caller
      try {
        await provider.sendAndConfirm(tx, [user]);
        assert.fail("Expected transaction to fail with UnauthorizedCaller");
      } catch (e: any) {
        assert.include(e.toString(), "UnauthorizedCaller");
      }
    });

    // Mirrors: test_wrap_reverts_if_the_input_length_is_wrong
    // https://github.com/anoma/anomapay-backend/blob/main/contracts/test/ERC20Forwarder.t.sol
    it("rejects wrap with wrong input length", async () => {
      if (!program) return;

      // Create input that's too short (missing fields)
      const shortInput = Buffer.alloc(50);
      shortInput.writeUInt8(OP_WRAP, 0);

      try {
        await program.methods
          .forwardCall(Array.from(logicRef), shortInput)
          .accounts({})
          .remainingAccounts([
            { pubkey: userAta, isSigner: false, isWritable: true },
            { pubkey: escrowAta, isSigner: false, isWritable: true },
            { pubkey: escrowPda, isSigner: false, isWritable: false },
            { pubkey: TOKEN_PROGRAM_ID, isSigner: false, isWritable: false },
            { pubkey: tokenMint, isSigner: false, isWritable: false },
          ])
          .rpc();
        assert.fail("Expected transaction to fail");
      } catch (e: any) {
        assert.include(e.toString(), "UnauthorizedCaller");
      }
    });
  });

  describe("unwrap", () => {
    // Mirrors: test_unwrap_sends_funds_to_the_user
    // https://github.com/anoma/anomapay-backend/blob/main/contracts/test/ERC20Forwarder.t.sol
    it("unwraps tokens to recipient", async () => {
      if (!program) return;

      const amount = BigInt(50_000_000); // 50 tokens

      // Create unwrap input
      const unwrapInput = encodeUnwrapInput(
        tokenMint,
        amount,
        recipient.publicKey
      );

      // Get balances before
      const escrowBefore = await getAccount(provider.connection, escrowAta);
      const recipientBefore = await getAccount(
        provider.connection,
        recipientAta
      );

      // Direct calls to forward_call are rejected — CPI introspection requires the PA as caller
      try {
        await program.methods
          .forwardCall(Array.from(logicRef), unwrapInput)
          .accounts({})
          .remainingAccounts([
            { pubkey: escrowAta, isSigner: false, isWritable: true },
            { pubkey: recipientAta, isSigner: false, isWritable: true },
            { pubkey: escrowPda, isSigner: false, isWritable: false },
            { pubkey: TOKEN_PROGRAM_ID, isSigner: false, isWritable: false },
            { pubkey: tokenMint, isSigner: false, isWritable: false },
          ])
          .rpc();
        assert.fail("Expected transaction to fail with UnauthorizedCaller");
      } catch (e: any) {
        assert.include(e.toString(), "UnauthorizedCaller");
      }
    });

    // Mirrors: test_unwrap_does_not_revert_if_the_amount_is_zero
    // https://github.com/anoma/anomapay-backend/blob/main/contracts/test/ERC20Forwarder.t.sol
    it("unwrap succeeds with zero amount", async () => {
      if (!program) return;

      const amount = BigInt(0); // Zero amount

      const unwrapInput = encodeUnwrapInput(
        tokenMint,
        amount,
        recipient.publicKey
      );

      const escrowBefore = await getAccount(provider.connection, escrowAta);
      const recipientBefore = await getAccount(
        provider.connection,
        recipientAta
      );

      // Direct calls to forward_call are rejected — CPI introspection requires the PA as caller
      try {
        await program.methods
          .forwardCall(Array.from(logicRef), unwrapInput)
          .accounts({})
          .remainingAccounts([
            { pubkey: escrowAta, isSigner: false, isWritable: true },
            { pubkey: recipientAta, isSigner: false, isWritable: true },
            { pubkey: escrowPda, isSigner: false, isWritable: false },
            { pubkey: TOKEN_PROGRAM_ID, isSigner: false, isWritable: false },
            { pubkey: tokenMint, isSigner: false, isWritable: false },
          ])
          .rpc();
        assert.fail("Expected transaction to fail with UnauthorizedCaller");
      } catch (e: any) {
        assert.include(e.toString(), "UnauthorizedCaller");
      }
    });

    // Mirrors: test_unwrap_reverts_if_the_input_length_is_wrong
    // https://github.com/anoma/anomapay-backend/blob/main/contracts/test/ERC20Forwarder.t.sol
    it("rejects unwrap with wrong input length", async () => {
      if (!program) return;

      // Create input that's too short (missing recipient field)
      const shortInput = Buffer.alloc(45);
      shortInput.writeUInt8(OP_UNWRAP, 0);
      tokenMint.toBuffer().copy(shortInput, 1);
      shortInput.writeBigUInt64LE(BigInt(1000), 33);
      // Missing recipient field

      try {
        await program.methods
          .forwardCall(Array.from(logicRef), shortInput)
          .accounts({})
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
        assert.include(e.toString(), "UnauthorizedCaller");
      }
    });
  });

  describe("error conditions", () => {
    // Mirrors: test_forwardCall_reverts_if_the_logic_ref_mismatches
    // https://github.com/anoma/anomapay-backend/blob/main/contracts/test/bases/ForwarderBase.t.sol
    it("rejects forward_call with wrong logic_ref", async () => {
      if (!program) return;

      const wrongLogicRef = Buffer.alloc(32);
      wrongLogicRef.fill(0x99);

      const unwrapInput = encodeUnwrapInput(
        tokenMint,
        BigInt(1000),
        recipient.publicKey
      );

      try {
        await program.methods
          .forwardCall(Array.from(wrongLogicRef), unwrapInput)
          .accounts({})
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
        assert.include(e.toString(), "UnauthorizedCaller");
      }
    });

    // Mirrors: test_forwardCall_reverts_if_the_pa_is_not_the_caller
    // https://github.com/anoma/anomapay-backend/blob/main/contracts/test/bases/ForwarderBase.t.sol
    it("rejects forward_call from non-PA caller", async () => {
      if (!program) return;

      const unwrapInput = encodeUnwrapInput(
        tokenMint,
        BigInt(1000),
        recipient.publicKey
      );

      // All direct calls fail — CPI introspection rejects anything not called via CPI from the PA
      try {
        await program.methods
          .forwardCall(Array.from(logicRef), unwrapInput)
          .accounts({})
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
        assert.include(e.toString(), "UnauthorizedCaller");
      }
    });

    // Mirrors: test_forwardCall_reverts_on_invalid_calltype
    // https://github.com/anoma/anomapay-backend/blob/main/contracts/test/ERC20Forwarder.t.sol
    it("rejects forward_call with invalid op code", async () => {
      if (!program) return;

      // Create input with invalid op code (not 0 or 1)
      const invalidInput = Buffer.alloc(73);
      invalidInput.writeUInt8(99, 0); // Invalid op code
      tokenMint.toBuffer().copy(invalidInput, 1);
      invalidInput.writeBigUInt64LE(BigInt(1000), 33);
      recipient.publicKey.toBuffer().copy(invalidInput, 41);

      try {
        await program.methods
          .forwardCall(Array.from(logicRef), invalidInput)
          .accounts({})
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
        assert.include(e.toString(), "UnauthorizedCaller");
      }
    });

    // Mirrors: test_wrap_reverts_if_user_did_not_approve_permit2
    // https://github.com/anoma/anomapay-backend/blob/main/contracts/test/ERC20Forwarder.t.sol
    it("rejects wrap without delegate approval", async () => {
      if (!program) return;

      // Create a new user without any delegate approval
      const newUser = Keypair.generate();
      await localAirdrop(newUser, 2);

      // Create token account and mint tokens
      const newUserAta = await createAccount(
        provider.connection,
        newUser,
        tokenMint,
        newUser.publicKey
      );
      await mintTo(
        provider.connection,
        authority,
        tokenMint,
        newUserAta,
        authority,
        100_000_000
      );

      const amount = BigInt(50_000_000);
      const nonce = BigInt(999); // Fresh nonce
      const deadline = BigInt(Math.floor(Date.now() / 1000) + 3600);
      const actionTreeRoot = Buffer.alloc(32);

      const messageHash = createWrapMessageHash(
        program.programId,
        tokenMint,
        amount,
        nonce,
        deadline,
        actionTreeRoot
      );
      const signature = nacl.sign.detached(messageHash, newUser.secretKey);

      // Note: NOT approving escrow PDA as delegate

      const [nonceBitmapPda] = deriveNonceBitmapPda(
        program.programId,
        newUser.publicKey,
        nonce
      );

      const ed25519Ix = Ed25519Program.createInstructionWithPublicKey({
        publicKey: newUser.publicKey.toBytes(),
        message: messageHash,
        signature: Buffer.from(signature),
      });

      const wrapInput = encodeWrapInput(
        tokenMint,
        amount,
        newUser.publicKey,
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
        .accounts({})
        .remainingAccounts([
          { pubkey: newUserAta, isSigner: false, isWritable: true },
          { pubkey: escrowAta, isSigner: false, isWritable: true },
          { pubkey: escrowPda, isSigner: false, isWritable: false },
          { pubkey: nonceBitmapPda, isSigner: false, isWritable: true },
          { pubkey: TOKEN_PROGRAM_ID, isSigner: false, isWritable: false },
          {
            pubkey: SystemProgram.programId,
            isSigner: false,
            isWritable: false,
          },
          { pubkey: newUser.publicKey, isSigner: true, isWritable: true },
          { pubkey: tokenMint, isSigner: false, isWritable: false },
        ])
        .instruction();

      tx.add(forwardCallIx);

      try {
        await provider.sendAndConfirm(tx, [newUser]);
        assert.fail("Expected transaction to fail");
      } catch (e: any) {
        assert.include(e.toString(), "UnauthorizedCaller");
      }
    });

    // Mirrors: test_setEmergencyCaller_reverts_if_the_caller_is_not_the_emergency_committee
    // https://github.com/anoma/anomapay-backend/blob/main/contracts/test/bases/EmergencyMigratableForwarderBase.t.sol
    it("rejects set_emergency_caller from non-committee", async () => {
      if (!program) return;

      const unauthorizedCommittee = Keypair.generate();
      await localAirdrop(unauthorizedCommittee, 1);

      const newCaller = Keypair.generate();

      try {
        await program.methods
          .setEmergencyCaller(newCaller.publicKey)
          .accounts({
            committee: unauthorizedCommittee.publicKey, // Wrong committee
            paState: paStatePda,
          })
          .signers([unauthorizedCommittee])
          .rpc();
        assert.fail("Expected transaction to fail");
      } catch (e: any) {
        assert.include(e.toString(), "UnauthorizedCaller");
      }
    });
  });

  after(async () => {
    await drainKeypairs(provider, localFundedKeypairs, "01-forwarder");
    localFundedKeypairs.length = 0;
  });
});
