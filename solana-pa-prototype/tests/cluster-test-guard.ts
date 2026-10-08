/**
 * The cluster test run (`ops.sh test --cluster <c>`) settles, uploads and
 * pays with its wallet against a live deployment. A wallet that holds an
 * owner's role, a program's upgrade authority or the adapter's stored owner,
 * could have a spec pause the deployment, replace its kind table, upgrade a
 * program or renounce the ownership for good (scripts/cluster-test-guard.ts).
 * The guard refuses such a wallet before any spec runs.
 */
import { Keypair } from "@solana/web3.js";
import { assert } from "chai";
import { ownerRoles, refuseOwnerWallet } from "../scripts/cluster-test-guard";
import { blockTimeForwarderId, provider, program, useAdapterSuite } from "./utils/adapterSuite";

describe("cluster test guard @localnet", () => {
  useAdapterSuite();
  const wallet = provider.wallet.publicKey;
  const targets = () => [program.programId, blockTimeForwarderId];
  const roles = () => ownerRoles(provider.connection, targets(), program);

  // The adapter's upgrade authority is its own PDA, which no wallet holds,
  // and its owner is stored; the block-time forwarder, which has no owner,
  // keeps the wallet as its upgrade authority.
  it("finds the stored owner and an upgrade authority that is not the program's PDA", async () => {
    assert.deepEqual(
      (await roles()).map((r) => [r.role, r.key.toBase58()]),
      [
        [`the upgrade authority of ${blockTimeForwarderId.toBase58()}`, wallet.toBase58()],
        [`the owner of the adapter ${program.programId.toBase58()}`, wallet.toBase58()],
      ],
    );
  });

  it("refuses a wallet that holds an owner's role, naming each role", async () => {
    const held = await roles();
    assert.throws(
      () => refuseOwnerWallet(wallet, held),
      new RegExp(
        `${wallet.toBase58()} is the upgrade authority of ${blockTimeForwarderId.toBase58()} ` +
          `and the owner of the adapter ${program.programId.toBase58()}`,
      ),
    );
  });

  it("accepts a wallet that holds no owner's role", async () => {
    refuseOwnerWallet(Keypair.generate().publicKey, await roles());
  });
});
