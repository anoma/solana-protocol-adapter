/**
 * SPL token forwarder: config initialization and the guards a direct caller
 * hits. Everything forward_call does past its caller check needs the
 * adapter as the CPI caller, so those behaviours are tested through
 * settlement in the adapter suite (tests/solana-pa-prototype.ts, the SPL
 * token forwarder block); the emergency flow is zz-forwarder-emergency.ts.
 *
 * Touches nothing of the adapter's state: the adapter suite's first test
 * needs an uninitialized adapter.
 */
import * as anchor from "@coral-xyz/anchor";
import { Program } from "@coral-xyz/anchor";
import { Keypair, PublicKey, SYSVAR_INSTRUCTIONS_PUBKEY } from "@solana/web3.js";
import { assert } from "chai";
import { ProtocolAdapter } from "../target/types/protocol_adapter";
import { SplTokenForwarder } from "../target/types/spl_token_forwarder";
import {
  EMERGENCY_COMMITTEE_LABEL,
  deriveConfigPda,
  derivePaStatePda,
  encodeUnwrapInput,
  makeFunder,
  requireFixture,
  seededKeypair,
} from "./utils";

describe("01-spl-token-forwarder (config and direct-call guards)", () => {
  const provider = anchor.AnchorProvider.env();
  anchor.setProvider(provider);

  const paProgram = anchor.workspace.ProtocolAdapter as Program<ProtocolAdapter>;
  const forwarderProgram = anchor.workspace.SplTokenForwarder as Program<SplTokenForwarder>;

  const [paState] = derivePaStatePda(paProgram.programId);
  const [configPda] = deriveConfigPda(forwarderProgram.programId);
  const funder = makeFunder(provider);

  const authority = Keypair.generate();
  const emergencyCommittee = seededKeypair(EMERGENCY_COMMITTEE_LABEL);

  // The logic ref the fixtures were proven under (the passthrough guest);
  // the config must authorize it for the adapter suite's wrap and unwrap.
  const logicRef = Buffer.from(
    requireFixture("spl_token_wrap.json", "--spl-token-wrap").spl_token_wrap!.logic_ref_b64,
    "base64"
  );

  before(async () => {
    await funder.fund(authority, 2);
    await funder.fund(emergencyCommittee, 1);
  });

  function initialize(protocolAdapter: PublicKey, logicRef: number[], committee: PublicKey) {
    return forwarderProgram.methods
      .initialize(protocolAdapter, logicRef, committee)
      .accounts({ authority: authority.publicKey })
      .signers([authority])
      .rpc();
  }

  describe("initialize", () => {
    // Mirrors ForwarderBase.t.sol: test_constructor_reverts_if_the_protocol_adapter_address_is_zero
    it("rejects a zero protocol adapter address", async () => {
      try {
        await initialize(PublicKey.default, Array.from(logicRef), emergencyCommittee.publicKey);
        assert.fail("expected initialize to fail");
      } catch (e: any) {
        assert.include(e.toString(), "ZeroAddressNotAllowed");
      }
    });

    // Mirrors ForwarderBase.t.sol: test_constructor_reverts_if_the_logic_ref_is_zero
    it("rejects a zero logic ref", async () => {
      try {
        await initialize(paProgram.programId, Array(32).fill(0), emergencyCommittee.publicKey);
        assert.fail("expected initialize to fail");
      } catch (e: any) {
        assert.include(e.toString(), "ZeroAddressNotAllowed");
      }
    });

    // Mirrors EmergencyMigratableForwarderBase.t.sol: test_constructor_reverts_if_the_emergency_committe_address_is_zero
    it("rejects a zero emergency committee", async () => {
      try {
        await initialize(paProgram.programId, Array.from(logicRef), PublicKey.default);
        assert.fail("expected initialize to fail");
      } catch (e: any) {
        assert.include(e.toString(), "ZeroAddressNotAllowed");
      }
    });

    // Mirrors ForwarderBase.t.sol getProtocolAdapter/getLogicRef and
    // EmergencyMigratableForwarderBase.t.sol emergencyCaller-is-zero-before-set.
    it("stores the adapter, logic ref and committee, with no emergency caller", async () => {
      await initialize(paProgram.programId, Array.from(logicRef), emergencyCommittee.publicKey);

      const config = await forwarderProgram.account.config.fetch(configPda);
      assert.ok(config.protocolAdapter.equals(paProgram.programId));
      assert.deepEqual(config.logicRef, Array.from(logicRef));
      assert.ok(config.emergencyCommittee.equals(emergencyCommittee.publicKey));
      assert.ok(config.emergencyCaller.equals(PublicKey.default));
    });
  });

  describe("guards", () => {
    // Mirrors ForwarderBase.t.sol: test_forwardCall_reverts_if_the_pa_is_not_the_caller.
    // The forwarder reads the current top-level instruction's program id from
    // the instructions sysvar; a direct call sees itself, not the adapter.
    it("rejects forward_call that is not a CPI from the adapter", async () => {
      const input = encodeUnwrapInput(Keypair.generate().publicKey, 1000n, Keypair.generate().publicKey);
      try {
        await forwarderProgram.methods
          .forwardCall(Array.from(logicRef), input)
          .accountsPartial({ config: configPda, ixSysvar: SYSVAR_INSTRUCTIONS_PUBKEY })
          .rpc();
        assert.fail("expected forward_call to fail");
      } catch (e: any) {
        assert.include(e.toString(), "UnauthorizedCaller");
      }
    });

    // Mirrors EmergencyMigratableForwarderBase.t.sol: test_setEmergencyCaller_reverts_if_the_caller_is_not_the_emergency_committee
    it("rejects set_emergency_caller from a non-committee signer", async () => {
      const impostor = Keypair.generate();
      await funder.fund(impostor, 1);

      try {
        await forwarderProgram.methods
          .setEmergencyCaller(Keypair.generate().publicKey)
          .accounts({ committee: impostor.publicKey, paState })
          .signers([impostor])
          .rpc();
        assert.fail("expected set_emergency_caller to fail");
      } catch (e: any) {
        assert.include(e.toString(), "UnauthorizedCaller");
      }
    });
  });

  after(() => funder.drainAll());
});
