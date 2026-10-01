/**
 * Renouncing an upgrade authority makes the program final for good, so these
 * run last on the suite's deployment: the forwarder's, then the adapter's,
 * after which no owner-only instruction can run.
 */
import { Transaction } from "@solana/web3.js";
import { assert } from "chai";
import { EMPTY_KIND_TABLE_COMMITMENT } from "../../client/constants";
import { setKindTableCommitment } from "../../client/instructions";
import { localSetUpgradeAuthority } from "../utils/localOnly";
import { assertFails } from "../utils/helpers";
import { refuseUpgradeAuthorityWallet, upgradeAuthority } from "../../scripts/cluster-test-guard";
import { provider, program, forwarderProgram, useAdapterSuite } from "../utils/adapterSuite";

describe("renounced upgrade authorities", () => {
  useAdapterSuite();
  const wallet = provider.wallet.publicKey;

  // The cluster test guard reads no upgrade authority from a final program,
  // so it accepts any wallet against it.
  it("cluster test guard: reads no upgrade authority from a final program, and accepts any wallet against it", async () => {
    await provider.sendAndConfirm(
      new Transaction().add(localSetUpgradeAuthority(provider.connection, forwarderProgram.programId, wallet, null)),
    );
    assert.isNull(await upgradeAuthority(provider.connection, forwarderProgram.programId));
    await refuseUpgradeAuthorityWallet(provider.connection, wallet, [forwarderProgram.programId]);
  });

  // Mirrors OwnableUpgradeable.renounceOwnership: with no upgrade authority
  // (the program final), every owner-only instruction is closed for good.
  it("protocol-adapter (authority): is renounced with the upgrade authority", async () => {
    await provider.sendAndConfirm(
      new Transaction().add(localSetUpgradeAuthority(provider.connection, program.programId, wallet, null)),
    );
    await assertFails(setKindTableCommitment(program, wallet, Array.from(EMPTY_KIND_TABLE_COMMITMENT)).rpc(), {
      program,
      error: "Unauthorized",
      account: "program_data",
    });
  });
});
