/**
 * SPL token forwarder config initialization: who may initialize, the
 * zero-value rejections and what a successful initialize stores. Needs no
 * config yet and touches nothing of the adapter: it runs first, on the fresh
 * deployment, and the config it stores is the one every later file uses.
 */
import { PublicKey } from "@solana/web3.js";
import { spawnSync } from "child_process";
import { assert } from "chai";
import { CONFIG_VERSION } from "../../client/constants";
import { initializeForwarder } from "../../client/instructions";
import { deriveConfigPda, deriveProgramDataPda, deriveUpgradeAuthorityPda } from "../../client/pda";
import { upgradeAuthority } from "../../client/upgrade";
import { makeFunder, randomRef, assertFails } from "../utils/helpers";
import {
  cpiEventsOf,
  forwarderLogicRef,
  forwarderCommittee as emergencyCommittee,
  forwarderProgram,
  program as paProgram,
  provider,
} from "../utils/adapterSuite";

describe("forwarder initialize", () => {
  const [configPda] = deriveConfigPda(forwarderProgram.programId);
  const funder = makeFunder(provider);

  const logicRef = forwarderLogicRef();

  // The program's upgrade authority, the deployer, initializes it, making
  // the provider wallet the owner: the EVM proxy runs its initializer
  // atomically at deployment, so no one else ever can.
  const wallet = provider.wallet.publicKey;
  const initialize = (adapter: PublicKey, ref: number[], committee: PublicKey, owner: PublicKey = wallet) =>
    initializeForwarder(forwarderProgram, adapter, ref, committee, owner, wallet).rpc();

  it("rejects an initialize signed by anyone but the program's upgrade authority", async () => {
    const intruder = await funder.fresh(2);
    await assertFails(
      initializeForwarder(
        forwarderProgram,
        paProgram.programId,
        logicRef,
        emergencyCommittee.publicKey,
        intruder.publicKey,
        intruder.publicKey,
      )
        .signers([intruder])
        .rpc(),
      { program: forwarderProgram, error: "UnauthorizedCaller", account: "program_data" },
    );
    assert.isNull(await provider.connection.getAccountInfo(configPda), "no config is created");
  });

  // Another program with the same upgrade authority is the cheapest forgery
  // of the upgrade-authority check.
  it("rejects the upgrade authority of another program's ProgramData", () =>
    assertFails(
      initializeForwarder(
        forwarderProgram,
        paProgram.programId,
        logicRef,
        emergencyCommittee.publicKey,
        wallet,
        wallet,
        deriveProgramDataPda(paProgram.programId),
      ).rpc(),
      { program: forwarderProgram, error: "UnauthorizedCaller", account: "program" },
    ));

  // Mirrors OwnableUpgradeable's initializer: OwnableInvalidOwner(address(0)).
  it("rejects a zero owner", () =>
    assertFails(initialize(paProgram.programId, logicRef, emergencyCommittee.publicKey, PublicKey.default), {
      program: forwarderProgram,
      error: "OwnableInvalidOwner",
    }));

  // Mirrors ForwarderBase.t.sol and EmergencyMigratableForwarderBase.t.sol:
  // test_constructor_reverts_if_the_{protocol_adapter_address,logic_ref,emergency_committe_address}_is_zero
  for (const [name, adapter, ref, committee] of [
    ["protocol adapter address", PublicKey.default, logicRef, emergencyCommittee.publicKey],
    ["logic ref", paProgram.programId, Array(32).fill(0), emergencyCommittee.publicKey],
    ["emergency committee", paProgram.programId, logicRef, PublicKey.default],
  ] as const) {
    it(`rejects a zero ${name}`, () =>
      assertFails(initialize(adapter, ref, committee), { program: forwarderProgram, error: "ZeroAddressNotAllowed" }));
  }

  // Mirrors ForwarderBase.t.sol getProtocolAdapter/getLogicRef and
  // EmergencyMigratableForwarderBase.t.sol emergencyCaller-is-zero-before-set.
  // Mirrors OpenZeppelin's initializer: the owner is set and announced first
  // (OwnershipTransferred from the zero address), then the config records
  // the version it was initialized at, this build's, and announces it. The
  // upgrade authority moves to the program, as a UUPS implementation
  // authorizes its own upgrades.
  it("stores the adapter, logic ref, committee and owner, with no emergency caller, at this build's version, and hands the upgrade authority to the program", async () => {
    const sig = await initialize(paProgram.programId, logicRef, emergencyCommittee.publicKey);

    const config = await forwarderProgram.account.config.fetch(configPda);
    assert.ok(config.protocolAdapter.equals(paProgram.programId));
    assert.deepEqual(config.logicRef, logicRef);
    assert.ok(config.emergencyCommittee.equals(emergencyCommittee.publicKey));
    assert.ok(config.emergencyCaller.equals(PublicKey.default));
    assert.equal(config.version.toNumber(), CONFIG_VERSION, "the config is at this build's version");
    assert.equal(config.owner.toBase58(), wallet.toBase58(), "the initial owner is stored");
    assert.equal(
      (await upgradeAuthority(provider.connection, forwarderProgram.programId))?.toBase58(),
      deriveUpgradeAuthorityPda(forwarderProgram.programId).toBase58(),
      "the program's upgrade authority is its PDA",
    );

    const { events } = await cpiEventsOf(sig, forwarderProgram);
    assert.deepEqual(
      events.map((e) =>
        e.name === "initialized"
          ? [e.name, e.data.version.toNumber()]
          : [e.name, e.data.previousOwner.toBase58(), e.data.newOwner.toBase58()],
      ),
      [
        ["ownershipTransferred", PublicKey.default.toBase58(), wallet.toBase58()],
        ["initialized", CONFIG_VERSION],
      ],
    );
  });

  // The operator's init is idempotent only for the config it would create.
  describe("the operator's init command", () => {
    const runInit = (ref: number[], committee: PublicKey) =>
      spawnSync("npx", ["ts-node", "-P", "tsconfig.json", "scripts/forwarder.ts", "init"], {
        env: {
          ...process.env,
          STF_LOGIC_REF: Buffer.from(ref).toString("hex"),
          STF_EMERGENCY_COMMITTEE: committee.toBase58(),
          STF_OWNER: wallet.toBase58(),
        },
        encoding: "utf-8",
      });

    it("accepts an existing config that holds the requested values", () => {
      const result = runInit(logicRef, emergencyCommittee.publicKey);
      assert.equal(result.status, 0, result.stderr);
      assert.include(result.stdout, "already initialized with the requested values");
    });

    it("refuses an existing config that differs from the request", () => {
      const result = runInit(randomRef(), emergencyCommittee.publicKey);
      assert.notEqual(result.status, 0, "init must fail");
      assert.include(result.stderr, "already exists with a different logic ref");
    });
  });

  after(() => funder.drainAll());
});
