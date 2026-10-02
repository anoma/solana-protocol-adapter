/**
 * Renouncing an ownership is for good, so these run last on the suite's
 * deployment: the forwarder's upgrade authority, then the adapter's
 * ownership, after which no owner-only instruction can run.
 */
import { Keypair, PublicKey, Transaction } from "@solana/web3.js";
import { assert } from "chai";
import { EMPTY_KIND_TABLE_COMMITMENT } from "../../client/constants";
import { setKindTableCommitment, upgradeAdapter } from "../../client/instructions";
import { upgradeAuthority } from "../../client/upgrade";
import { localRenounceAdapterOwnership, localSetUpgradeAuthority } from "../utils/localOnly";
import { assertFails } from "../utils/helpers";
import { ownerRoles, refuseOwnerWallet } from "../../scripts/cluster-test-guard";
import { cpiEventsOf, provider, program, paState, forwarderProgram, useAdapterSuite } from "../utils/adapterSuite";

describe("renounced ownerships", () => {
  useAdapterSuite();
  const wallet = provider.wallet.publicKey;

  // The cluster test guard reads no upgrade authority from a final program,
  // so it finds no owner's role there.
  it("cluster test guard: reads no upgrade authority from a final program", async () => {
    await provider.sendAndConfirm(
      new Transaction().add(localSetUpgradeAuthority(provider.connection, forwarderProgram.programId, wallet, null)),
    );
    assert.isNull(await upgradeAuthority(provider.connection, forwarderProgram.programId));
    const roles = await ownerRoles(provider.connection, [forwarderProgram.programId], program);
    assert.notInclude(
      roles.map((r) => r.role),
      `the upgrade authority of ${forwarderProgram.programId.toBase58()}`,
    );
  });

  // Mirrors OwnableUpgradeable.renounceOwnership: the owner becomes the zero
  // address, announced, and every owner-only instruction, upgrades included,
  // is closed for good.
  it("protocol-adapter (ownership): is renounced, announced, and closes every owner-only instruction", async () => {
    const sig = await localRenounceAdapterOwnership(program, wallet).rpc();
    const { events } = await cpiEventsOf(sig);
    assert.deepEqual(
      events.map((e) => [e.name, e.data.previousOwner.toBase58(), e.data.newOwner.toBase58()]),
      [["ownershipTransferredEvent", wallet.toBase58(), PublicKey.default.toBase58()]],
    );
    assert.equal((await program.account.paStateAccount.fetch(paState)).owner.toBase58(), PublicKey.default.toBase58());

    await assertFails(setKindTableCommitment(program, wallet, Array.from(EMPTY_KIND_TABLE_COMMITMENT)).rpc(), {
      program,
      error: "OwnableUnauthorizedAccount",
      account: "authority",
    });
    await assertFails(upgradeAdapter(program, wallet, Keypair.generate().publicKey, wallet).rpc(), {
      program,
      error: "OwnableUnauthorizedAccount",
      account: "authority",
    });
    refuseOwnerWallet(wallet, await ownerRoles(provider.connection, [program.programId], program));
  });
});
