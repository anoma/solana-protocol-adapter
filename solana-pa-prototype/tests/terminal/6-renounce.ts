/**
 * Renouncing the adapter's ownership is for good, so it runs last on the
 * suite's deployment, after which no owner-only instruction can run.
 */
import { Keypair, PublicKey } from "@solana/web3.js";
import { assert } from "chai";
import { EMPTY_KIND_TABLE_COMMITMENT } from "../../client/constants";
import { setKindTableCommitment, upgradeAdapter } from "../../client/instructions";
import { localRenounceAdapterOwnership } from "../utils/localOnly";
import { assertFails } from "../utils/helpers";
import { ownerRoles, refuseOwnerWallet } from "../../scripts/cluster-test-guard";
import { cpiEventsOf, provider, program, paState, forwarderProgram, useAdapterSuite } from "../utils/adapterSuite";

describe("renounced adapter ownership", () => {
  useAdapterSuite();
  const wallet = provider.wallet.publicKey;

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
    refuseOwnerWallet(
      wallet,
      await ownerRoles(provider.connection, [program.programId, forwarderProgram.programId], program, forwarderProgram),
    );
  });
});
