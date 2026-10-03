/**
 * The SPL token forwarder upgraded in place across its config layout change,
 * as the EVM forwarder's UUPS proxy is upgraded: its validator starts on the
 * build deployed on devnet (tests/fixtures/previous/spl_token_forwarder.so,
 * whose owner is the program's upgrade authority), which initializes its
 * config and a nonce bitmap. The program is upgraded through the loader to
 * this build, and `migrate_config` brings the config to this layout, owned
 * by the upgrade authority, and hands the upgrade authority to the
 * program's PDA, after which the owner alone runs it and upgrades it.
 */
import { BN, Idl, Program } from "@anchor-lang/core";
import { Keypair, PublicKey } from "@solana/web3.js";
import { assert } from "chai";
import { readFileSync } from "fs";
import { reinitializeForwarder, upgradeForwarder } from "../../client/instructions";
import {
  deriveConfigPda,
  deriveNonceBitmapPda,
  deriveProgramDataPda,
  deriveUpgradeAuthorityPda,
} from "../../client/pda";
import { upgradeAuthority } from "../../client/upgrade";
import { assertFails, randomRef, solanaCli, waitForSlotPast } from "../utils/helpers";
import { localMigrateConfig } from "../utils/localOnly";
import { FORWARDER_SO } from "../utils/constants";
import {
  cpiEventsOf,
  forwarderProgram,
  program as adapter,
  provider,
  upgradeThroughProgram,
  useAdapterSuite,
} from "../utils/adapterSuite";

/** The previous build, through its production IDL (anoma-pa-solana-client's copy of that build's). */
const previous: Program<any> = new Program(
  JSON.parse(readFileSync("tests/fixtures/previous/spl_token_forwarder.json", "utf8")) as Idl,
  provider,
);

describe("spl-token-forwarder (upgraded in place across its config layout)", () => {
  const { funder } = useAdapterSuite();
  const wallet = provider.wallet.publicKey;
  const [configPda] = deriveConfigPda(forwarderProgram.programId);
  const logicRef = randomRef();
  const committee = Keypair.generate().publicKey;
  const bitmapUser = Keypair.generate().publicKey;
  const [bitmap] = deriveNonceBitmapPda(forwarderProgram.programId, bitmapUser, 0n);
  let configBefore: any;

  it("initializes the previous build's config and a nonce bitmap", async () => {
    await previous.methods
      .initialize(adapter.programId, logicRef, committee)
      .accountsPartial({ authority: wallet, programData: deriveProgramDataPda(forwarderProgram.programId) })
      .rpc();
    await previous.methods.initNonceBitmap(bitmapUser, new BN(0)).accountsPartial({ payer: wallet }).rpc();
    configBefore = await (previous.account as any).config.fetch(configPda);
    assert.equal(
      (await provider.connection.getAccountInfo(configPda))!.data.length,
      8 + 32 * 4 + 8,
      "the previous build's config has no owner",
    );
  });

  // The loader's new code runs from the slot after the upgrade.
  it("upgrades the program in place to this build through the loader", async () => {
    solanaCli(
      provider,
      "program",
      "deploy",
      FORWARDER_SO,
      "--program-id",
      "target/deploy/spl_token_forwarder-keypair.json",
    );
    await waitForSlotPast(provider.connection, await provider.connection.getSlot("confirmed"));
  });

  // The previous config lacks the owner's 32 bytes, so it does not read as
  // this layout, and nothing misreads it.
  it("refuses an owner-only instruction before the config is migrated", () =>
    assertFails(reinitializeForwarder(forwarderProgram, wallet, randomRef()).rpc(), {
      program: forwarderProgram,
      error: "AccountDidNotDeserialize",
      account: "config",
    }));

  // Only the upgrade authority migrates, as only the EVM proxy's owner calls
  // upgradeToAndCall.
  it("rejects the migration from anyone but the upgrade authority", async () => {
    const stranger = await funder.fresh(1);
    await assertFails(localMigrateConfig(forwarderProgram, stranger.publicKey).signers([stranger]).rpc(), {
      program: forwarderProgram,
      error: "Unauthorized",
      account: "program_data",
    });
  });

  it("migrates the config in place, owned by the upgrade authority, and hands the upgrade authority to the program", async () => {
    const sig = await localMigrateConfig(forwarderProgram, wallet).rpc();
    const { events } = await cpiEventsOf(sig, forwarderProgram);
    assert.deepEqual(
      events.map((e) => [e.name, e.data.previousOwner.toBase58(), e.data.newOwner.toBase58()]),
      [["ownershipTransferred", PublicKey.default.toBase58(), wallet.toBase58()]],
      "the migration announces the owner, as initialize does",
    );

    const config: any = await forwarderProgram.account.config.fetch(configPda);
    assert.equal(config.owner.toBase58(), wallet.toBase58(), "the upgrade authority is the stored owner");
    for (const field of ["protocolAdapter", "logicRef", "emergencyCommittee", "emergencyCaller", "version"]) {
      assert.equal(JSON.stringify(config[field]), JSON.stringify(configBefore[field]), `${field} carries over`);
    }
    assert.equal(
      (await upgradeAuthority(provider.connection, forwarderProgram.programId))?.toBase58(),
      deriveUpgradeAuthorityPda(forwarderProgram.programId).toBase58(),
      "the program's upgrade authority is its PDA",
    );
    assert.deepEqual(
      (await forwarderProgram.account.nonceBitmap.fetch(bitmap)).bits,
      new Array(32).fill(0),
      "the nonce bitmap, whose layout is unchanged, reads under this build",
    );
  });

  // The upgrade authority is the program's PDA now, so the loader refuses
  // the wallet: the migration cannot run twice, and the loader path is shut.
  it("rejects migrating twice", () =>
    assertFails(localMigrateConfig(forwarderProgram, wallet).rpc(), {
      program: forwarderProgram,
      error: "Unauthorized",
      account: "program_data",
    }));

  it("upgrades through the program from then on", async () => {
    await upgradeThroughProgram(forwarderProgram, FORWARDER_SO, "upgraded", (buffer, spill) =>
      upgradeForwarder(forwarderProgram, wallet, buffer, spill),
    );
    await assertFails(reinitializeForwarder(forwarderProgram, wallet, randomRef()).rpc(), {
      program: forwarderProgram,
      error: "InvalidInitialization",
    });
  });
});
