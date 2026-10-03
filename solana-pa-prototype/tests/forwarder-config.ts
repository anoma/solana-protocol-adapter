/**
 * SPL token forwarder config: who may reinitialize it, and the guards a
 * direct caller hits, on the config the deployment runs. Everything
 * forward_call does past its caller check needs the adapter as the CPI
 * caller, so those behaviours are tested through settlement
 * (spl-token-wrap-unwrap.ts); the emergency flow is forwarder-emergency.ts.
 */
import { BN } from "@anchor-lang/core";
import { Keypair, PublicKey, SYSVAR_INSTRUCTIONS_PUBKEY } from "@solana/web3.js";
import { assert } from "chai";
import { encodeUnwrapInput, reinitializeForwarder } from "../client/instructions";
import { localSetEmergencyCaller } from "./utils/localOnly";
import { CONFIG_VERSION, OP_UNWRAP } from "../client/constants";
import { deriveConfigPda, deriveProgramDataPda } from "../client/pda";
import { makeFunder, randomRef, assertFails } from "./utils/helpers";
import { ensureForwarderConfig, forwarderProgram, paState, program as paProgram, provider } from "./utils/adapterSuite";

describe("forwarder config (logic ref and direct-call guards) @localnet", () => {
  const [configPda] = deriveConfigPda(forwarderProgram.programId);
  const funder = makeFunder(provider);
  // The logic ref the deployment's config serves; the rotation test puts it back.
  let logicRef: number[];

  before(async () => {
    logicRef = (await ensureForwarderConfig()).logicRef;
  });

  describe("reinitialize", () => {
    const reinitialize = (ref: number[], programData?: PublicKey) =>
      reinitializeForwarder(forwarderProgram, provider.wallet.publicKey, ref, programData).rpc();
    // The development build's dev_set_config_version, looked up untyped, as
    // production types lack it: it puts the config below this build's
    // version, as an earlier build would have left it.
    const lowerConfigVersion = () =>
      (
        (forwarderProgram.methods as Record<string, unknown>).devSetConfigVersion as (
          version: BN,
        ) => ReturnType<typeof forwarderProgram.methods.reinitialize>
      )(new BN(CONFIG_VERSION - 1))
        .accountsPartial({
          authority: provider.wallet.publicKey,
          programData: deriveProgramDataPda(forwarderProgram.programId),
        })
        .rpc();

    // Every later file wraps under the logic ref the deployment served.
    after(async () => {
      if (
        !Buffer.from((await forwarderProgram.account.config.fetch(configPda)).logicRef).equals(Buffer.from(logicRef))
      ) {
        await lowerConfigVersion();
        await reinitialize(logicRef);
      }
    });

    it("rejects a signer that is not the program's upgrade authority", async () => {
      const impostor = await funder.fresh(1);
      await assertFails(
        reinitializeForwarder(forwarderProgram, impostor.publicKey, randomRef()).signers([impostor]).rpc(),
        { program: forwarderProgram, error: "UnauthorizedCaller", account: "program_data" },
      );
      assert.deepEqual(
        (await forwarderProgram.account.config.fetch(configPda)).logicRef,
        logicRef,
        "the config is untouched",
      );
    });

    // The upgrade-authority check reads whatever ProgramData is passed; the
    // program constraint pins it to this program's own. Another program with
    // the same upgrade authority is the cheapest forgery.
    it("rejects the upgrade authority of another program's ProgramData", () =>
      assertFails(reinitialize(randomRef(), deriveProgramDataPda(paProgram.programId)), {
        program: forwarderProgram,
        error: "UnauthorizedCaller",
        account: "program",
      }));

    // Mirrors OpenZeppelin's reinitializer(n): InvalidInitialization once the
    // version is n. A config this build initialized is at its version, so
    // rotating it takes a build that raises CONFIG_VERSION.
    it("rejects a config already at this build's version", async () => {
      await assertFails(reinitialize(randomRef()), { program: forwarderProgram, error: "InvalidInitialization" });
      assert.deepEqual(
        (await forwarderProgram.account.config.fetch(configPda)).logicRef,
        logicRef,
        "the config is untouched",
      );
    });

    // A config an earlier build initialized sits below this build's
    // CONFIG_VERSION; reinitialize then rotates the ref and records the
    // version, once.
    it("rotates the logic ref once for a config below this build's version", async () => {
      await lowerConfigVersion();
      const rotated = randomRef();
      await reinitialize(rotated);
      const config = await forwarderProgram.account.config.fetch(configPda);
      assert.deepEqual(config.logicRef, rotated, "the logic ref is rotated");
      assert.equal(config.version.toNumber(), CONFIG_VERSION, "the config records this build's version");
      await assertFails(reinitialize(randomRef()), { program: forwarderProgram, error: "InvalidInitialization" });
    });
  });

  describe("guards", () => {
    // Mirrors ForwarderBase.t.sol: test_forwardCall_reverts_if_the_pa_is_not_the_caller.
    // The forwarder reads the current top-level instruction's program id from
    // the instructions sysvar; a direct call sees itself, not the adapter.
    it("rejects forward_call that is not a CPI from the adapter", () => {
      const operand = encodeUnwrapInput(Keypair.generate().publicKey, 1000n, Keypair.generate().publicKey);
      return assertFails(
        forwarderProgram.methods
          .forwardCall(logicRef, Buffer.concat([Buffer.from([OP_UNWRAP]), operand]))
          .accountsPartial({ config: configPda, ixSysvar: SYSVAR_INSTRUCTIONS_PUBKEY })
          .rpc(),
        { program: forwarderProgram, error: "UnauthorizedCaller" },
      );
    });

    // Mirrors EmergencyMigratableForwarderBase.t.sol: test_setEmergencyCaller_reverts_if_the_caller_is_not_the_emergency_committee
    it("rejects set_emergency_caller from a non-committee signer", async () => {
      const impostor = await funder.fresh(1);
      await assertFails(
        localSetEmergencyCaller(forwarderProgram, impostor.publicKey, paState, Keypair.generate().publicKey)
          .signers([impostor])
          .rpc(),
        { program: forwarderProgram, error: "UnauthorizedCaller" },
      );
    });
  });

  after(() => funder.drainAll());
});
