/**
 * `idl-publish` through client/programMetadata.ts: a program's upgrade
 * authority creates its canonical IDL account and makes the owner its
 * explicit authority; once the upgrade authority is a key the owner cannot
 * sign for (a program's own PDA, after `initialize`), the owner still updates
 * the account as its explicit authority. The Program Metadata CLI refuses that
 * update, which is why the library writes it. Exercised on the localnet test
 * forwarder, whose upgrade authority the provider wallet holds at genesis.
 */
import { mkdtempSync, readFileSync, writeFileSync } from "fs";
import { tmpdir } from "os";
import { join } from "path";
import { address, createSolanaRpc } from "@solana/kit";
import { Keypair, Transaction } from "@solana/web3.js";
import { assert } from "chai";
import { fetchMetadataContent } from "@solana-program/program-metadata";
import { publishIdl } from "../client/programMetadata";
import { upgradeAuthority } from "../client/upgrade";
import { localSetIdlAuthority, localSetUpgradeAuthorityIx } from "./utils/localOnly";
import { provider, testForwarderProgram, useAdapterSuite } from "./utils/adapterSuite";

describe("idl-publish @localnet", () => {
  useAdapterSuite();
  const rpcUrl = provider.connection.rpcEndpoint;
  const wallet = process.env.ANCHOR_WALLET!;
  const program = testForwarderProgram.programId;

  /** The test forwarder's IDL with `version` as its metadata version, written to a file of its own. */
  const idlFile = (version: string) => {
    const idl = JSON.parse(readFileSync("target/idl/test_forwarder.json", "utf8"));
    const path = join(mkdtempSync(join(tmpdir(), "idl-publish-")), "test_forwarder.json");
    writeFileSync(path, JSON.stringify({ ...idl, metadata: { ...idl.metadata, version } }));
    return path;
  };
  /** The IDL version the cluster serves for the test forwarder. */
  const servedVersion = async () => {
    return JSON.parse(await fetchMetadataContent(createSolanaRpc(rpcUrl), address(program.toBase58()), "idl")).metadata
      .version;
  };

  it("creates the canonical IDL account as the program's upgrade authority", async () => {
    assert.equal(
      (await upgradeAuthority(provider.connection, program))?.toBase58(),
      provider.wallet.publicKey.toBase58(),
    );
    assert.equal(await publishIdl(rpcUrl, wallet, idlFile("1.0.0")), "upgrade authority");
    assert.equal(await servedVersion(), "1.0.0");
  });

  it("updates it as the account's explicit authority once the wallet no longer holds the upgrade authority", async () => {
    await localSetIdlAuthority(rpcUrl, wallet, program, provider.wallet.publicKey);
    const elsewhere = Keypair.generate().publicKey;
    await provider.sendAndConfirm(
      new Transaction().add(
        localSetUpgradeAuthorityIx(provider.connection, program, provider.wallet.publicKey, elsewhere),
      ),
    );
    assert.equal((await upgradeAuthority(provider.connection, program))?.toBase58(), elsewhere.toBase58());

    assert.equal(await publishIdl(rpcUrl, wallet, idlFile("2.0.0")), "metadata authority");
    assert.equal(await servedVersion(), "2.0.0");
  });

  it("refuses a signer that is neither, before sending anything", async () => {
    const stranger = Keypair.generate();
    const strangerFile = join(mkdtempSync(join(tmpdir(), "idl-publish-")), "stranger.json");
    writeFileSync(strangerFile, JSON.stringify(Array.from(stranger.secretKey)));
    let refusal = "";
    try {
      await publishIdl(rpcUrl, strangerFile, idlFile("3.0.0"));
    } catch (e) {
      refusal = (e as Error).message;
    }
    assert.match(refusal, /can write neither/);
    assert.equal(await servedVersion(), "2.0.0");
  });
});
