/**
 * The cluster test run (`ops.sh test --cluster <c>`) settles, uploads and
 * pays with its wallet against a live deployment. A wallet that is a target
 * program's upgrade authority is that program's owner, so any spec it runs
 * could pause the deployment, replace its kind table, or renounce the
 * authority and leave the program final for good (scripts/cluster-test-guard.ts).
 * The guard refuses such a wallet before any spec runs.
 */
import { Keypair, Transaction } from "@solana/web3.js";
import { assert } from "chai";
import { localSetUpgradeAuthority } from "./utils/localOnly";
import { refuseUpgradeAuthorityWallet, upgradeAuthority } from "../scripts/cluster-test-guard";
import { provider, program, forwarderProgram, useAdapterSuite } from "./utils/adapterSuite";

describe("cluster test guard", () => {
  useAdapterSuite();
  const wallet = provider.wallet.publicKey;
  const targets = () => [program.programId, forwarderProgram.programId];

  it("reads each program's upgrade authority from its ProgramData", async () => {
    for (const id of targets()) {
      const authority = await upgradeAuthority(provider.connection, id);
      assert.isNotNull(authority, `${id.toBase58()} has an upgrade authority on the test validator`);
      assert.equal(authority!.toBase58(), wallet.toBase58());
    }
  });

  it("refuses a wallet that is a target program's upgrade authority, naming the program", async () => {
    let error: Error | undefined;
    try {
      await refuseUpgradeAuthorityWallet(provider.connection, wallet, targets());
    } catch (e) {
      error = e as Error;
    }
    assert.isDefined(error, "the guard accepted the upgrade-authority wallet");
    assert.include(error!.message, wallet.toBase58());
    assert.include(error!.message, program.programId.toBase58());
  });

  it("accepts a wallet that is no target program's upgrade authority", async () => {
    await refuseUpgradeAuthorityWallet(provider.connection, Keypair.generate().publicKey, targets());
  });

  it("reads no upgrade authority from a final program, and accepts any wallet against it", async () => {
    await provider.sendAndConfirm(
      new Transaction().add(localSetUpgradeAuthority(provider.connection, forwarderProgram.programId, wallet, null)),
    );
    assert.isNull(await upgradeAuthority(provider.connection, forwarderProgram.programId));
    await refuseUpgradeAuthorityWallet(provider.connection, wallet, [forwarderProgram.programId]);
  });
});
