/**
 * `idl-publish` through client/programMetadata.ts: a program's upgrade
 * authority creates its canonical IDL account, and no other signer writes it,
 * so once the upgrade authority is a key the wallet cannot sign for (a
 * program's own PDA, after `initialize`), the account keeps what was
 * published. Exercised on the localnet test forwarder, whose upgrade authority
 * the provider wallet holds at genesis.
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
import { localSetUpgradeAuthorityIx } from "./utils/localOnly";
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
    await publishIdl(rpcUrl, wallet, idlFile("1.0.0"));
    assert.equal(await servedVersion(), "1.0.0");
  });

  it("refuses the wallet once it no longer holds the upgrade authority, before sending anything", async () => {
    const elsewhere = Keypair.generate().publicKey;
    await provider.sendAndConfirm(
      new Transaction().add(
        localSetUpgradeAuthorityIx(provider.connection, program, provider.wallet.publicKey, elsewhere),
      ),
    );
    assert.equal((await upgradeAuthority(provider.connection, program))?.toBase58(), elsewhere.toBase58());

    let refusal = "";
    try {
      await publishIdl(rpcUrl, wallet, idlFile("2.0.0"));
    } catch (e) {
      refusal = (e as Error).message;
    }
    assert.match(refusal, new RegExp(`cannot write .*upgrade authority is ${elsewhere.toBase58()}`));
    assert.equal(await servedVersion(), "1.0.0");
  });
});
