/**
 * SPL token forwarder config: who may reinitialize it, and the guards a
 * direct caller hits, on the config the deployment runs. Everything
 * forward_call does past its caller check needs the adapter as the CPI
 * caller, so those behaviours are tested through settlement
 * (fresh/4-spl-token-wrap-unwrap.ts); the emergency flow is
 * terminal/4-forwarder-emergency.ts.
 */
import { BN } from "@anchor-lang/core";
import { Keypair, SYSVAR_INSTRUCTIONS_PUBKEY } from "@solana/web3.js";
import { assert } from "chai";
import { encodeUnwrapInput, reinitializeForwarder } from "../client/instructions";
import { localSetEmergencyCaller } from "./utils/localOnly";
import { CONFIG_VERSION, OP_UNWRAP } from "../client/constants";
import { deriveConfigPda } from "../client/pda";
import { makeFunder, randomRef, assertFails } from "./utils/helpers";
import { ensureForwarderConfig, forwarderProgram, paState, provider } from "./utils/adapterSuite";

describe("forwarder config (logic ref and direct-call guards) @localnet", () => {
  const [configPda] = deriveConfigPda(forwarderProgram.programId);
  const funder = makeFunder(provider);
  // The logic ref the deployment's config serves; the rotation test puts it back.
  let logicRef: number[];

  before(async () => {
    logicRef = (await ensureForwarderConfig()).logicRef;
  });

  describe("reinitialize", () => {
    const reinitialize = (ref: number[]) =>
      reinitializeForwarder(forwarderProgram, provider.wallet.publicKey, ref).rpc();
    // The development build's dev_set_config_version, looked up untyped, as
    // production types lack it: it puts the config below this build's
    // version, as an earlier build would have left it.
    const lowerConfigVersion = () =>
      (
        (forwarderProgram.methods as Record<string, unknown>).devSetConfigVersion as (
          version: BN,
        ) => ReturnType<typeof forwarderProgram.methods.reinitialize>
      )(new BN(CONFIG_VERSION - 1))
        .accountsPartial({ authority: provider.wallet.publicKey })
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

    // Only the owner rotates, as only the EVM forwarder's owner calls
    // upgradeToAndCall with the reinitializer.
    it("rejects a signer that is not the owner", async () => {
      const impostor = await funder.fresh(1);
      await assertFails(
        reinitializeForwarder(forwarderProgram, impostor.publicKey, randomRef()).signers([impostor]).rpc(),
        { program: forwarderProgram, error: "OwnableUnauthorizedAccount", account: "authority" },
      );
      assert.deepEqual(
        (await forwarderProgram.account.config.fetch(configPda)).logicRef,
        logicRef,
        "the config is untouched",
      );
    });

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
