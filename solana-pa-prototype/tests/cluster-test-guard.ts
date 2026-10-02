/**
 * The cluster test run (`ops.sh test --cluster <c>`) settles, uploads and
 * pays with its wallet against a live deployment. A wallet that holds an
 * owner's role, a program's upgrade authority or the adapter's stored
 * owner, could have a spec pause the deployment, replace its kind table,
 * upgrade a program or renounce the ownership for good
 * (scripts/cluster-test-guard.ts). The guard refuses such a wallet before
 * any spec runs.
 */
import { Keypair } from "@solana/web3.js";
import { assert } from "chai";
import { ownerRoles, refuseOwnerWallet } from "../scripts/cluster-test-guard";
import { provider, program, forwarderProgram, useAdapterSuite } from "./utils/adapterSuite";

describe("cluster test guard @localnet", () => {
  useAdapterSuite();
  const wallet = provider.wallet.publicKey;
  const targets = () => [program.programId, forwarderProgram.programId];

  // The adapter's upgrade authority is its own PDA, which no wallet holds;
  // its owner is stored. The forwarder's upgrade authority is the wallet.
  it("finds the adapter's stored owner and the forwarder's upgrade authority, not the adapter's PDA", async () => {
    const roles = await ownerRoles(provider.connection, targets(), program);
    assert.deepEqual(
      roles.map((r) => [r.role, r.key.toBase58()]),
      [
        [`the upgrade authority of ${forwarderProgram.programId.toBase58()}`, wallet.toBase58()],
        [`the owner of the adapter ${program.programId.toBase58()}`, wallet.toBase58()],
      ],
    );
  });

  it("refuses a wallet that holds an owner's role, naming each role", async () => {
    const roles = await ownerRoles(provider.connection, targets(), program);
    assert.throws(
      () => refuseOwnerWallet(wallet, roles),
      new RegExp(
        `${wallet.toBase58()} is the upgrade authority of ${forwarderProgram.programId.toBase58()} ` +
          `and the owner of the adapter ${program.programId.toBase58()}`,
      ),
    );
  });

  it("accepts a wallet that holds no owner's role", async () => {
    refuseOwnerWallet(Keypair.generate().publicKey, await ownerRoles(provider.connection, targets(), program));
  });
});
