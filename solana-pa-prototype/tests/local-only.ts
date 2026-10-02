// Every builder in tests/utils/localOnly.ts refuses a non-local RPC endpoint and works against the local validator,
// and no other code in the repository builds those instructions.
import { readdirSync, readFileSync } from "fs";
import { join } from "path";
import { AnchorProvider, Program } from "@anchor-lang/core";
import { Connection, Keypair } from "@solana/web3.js";
import { assert } from "chai";
import { ProtocolAdapter } from "../target/types/protocol_adapter";
import { SplTokenForwarder } from "../target/types/spl_token_forwarder";
import {
  localCloseAllMarkers,
  localCloseAllNonceBitmaps,
  localCloseConfig,
  localCloseEscrow,
  localCloseMarkersBatch,
  localCloseNonceBitmapsBatch,
  localMigrateState,
  localRenounceAdapterOwnership,
  localSetEmergencyCaller,
  localTransferAdapterOwnership,
  localSetUpgradeAuthority,
} from "./utils/localOnly";
import { forwarderProgram, paState, program, provider, useAdapterSuite } from "./utils/adapterSuite";

const LIVE_ENDPOINTS = [
  "https://api.devnet.solana.com",
  "https://api.mainnet-beta.solana.com",
  "https://devnet.helius-rpc.com/?api-key=k",
  "http://10.0.0.5:8899",
  "http://127.0.0.1.example.com:8899",
];

describe("authority and closing instructions are local-only", () => {
  useAdapterSuite();
  const someone = () => Keypair.generate().publicKey;
  const escrowAccounts = () => ({ mint: someone(), escrowAta: someone(), recipientAta: someone() });

  /** Every builder, bound to the programs it would send through. */
  const builders = (adapter: Program<ProtocolAdapter>, forwarder: Program<SplTokenForwarder>) => ({
    "transfer the adapter's ownership": () => localTransferAdapterOwnership(adapter, someone(), someone()),
    "renounce the adapter's ownership": () => localRenounceAdapterOwnership(adapter, someone()),
    "migrate the adapter's state": () => localMigrateState(adapter, someone()),
    "set the upgrade authority": () =>
      localSetUpgradeAuthority(adapter.provider.connection, adapter.programId, someone(), null),
    "set the emergency caller": () => localSetEmergencyCaller(forwarder, someone(), paState, someone()),
    "close an escrow": () => localCloseEscrow(forwarder, someone(), paState, escrowAccounts()),
    "close the forwarder config": () => localCloseConfig(forwarder, someone(), paState),
    "close nonce bitmaps": () => localCloseNonceBitmapsBatch(forwarder, someone(), paState, [someone()]),
    "close markers": () => localCloseMarkersBatch(adapter, someone(), [someone()]),
  });

  for (const endpoint of LIVE_ENDPOINTS) {
    const live = new AnchorProvider(new Connection(endpoint), provider.wallet, {});
    const liveAdapter = () => new Program<ProtocolAdapter>(program.idl, live);
    const liveForwarder = () => new Program<SplTokenForwarder>(forwarderProgram.idl, live);

    it(`refuses every builder against ${endpoint}`, async () => {
      for (const [name, build] of Object.entries(builders(liveAdapter(), liveForwarder()))) {
        assert.throws(build, /local validator/, `${name} was built against ${endpoint}`);
      }
      for (const [name, run] of Object.entries({
        "close all nonce bitmaps": () => localCloseAllNonceBitmaps(liveForwarder(), someone(), paState, []),
        "close all markers": () => localCloseAllMarkers(liveAdapter(), someone()),
      })) {
        let refused = false;
        try {
          await run();
        } catch (e) {
          refused = /local validator/.test((e as Error).message);
        }
        assert.isTrue(refused, `${name} ran against ${endpoint}`);
      }
    });
  }

  it("builds every one against the local validator @localnet", () => {
    for (const [name, build] of Object.entries(builders(program, forwarderProgram))) {
      assert.isOk(build(), name);
    }
  });
});

describe("no code outside tests/utils/localOnly.ts changes an authority or closes protocol accounts", () => {
  const ROOT = join(__dirname, "..");
  const LOCAL_ONLY = "tests/utils/localOnly.ts";
  /** The forwarder and adapter instructions localOnly.ts alone may build, and the loader's SetAuthority. */
  const TS_AUTHORITY_CALL =
    /\.(transferOwnership|renounceOwnership|migrateState|setEmergencyCaller|closeEscrow|closeConfig|closeNonceBitmapsBatch|closeMarkersBatch)\s*\(|programId:\s*BPF_LOADER_UPGRADEABLE/;
  /** The Solana CLI commands that move or renounce an authority. */
  const SHELL_AUTHORITY_CALL = /\bset-upgrade-authority\b|\bset-buffer-authority\b|\bset-authority\b|--final\b/;

  const files = (dir: string, ext: string): string[] =>
    readdirSync(join(ROOT, dir), { recursive: true, encoding: "utf8" })
      .filter((p) => p.endsWith(ext))
      .map((p) => join(dir, p));
  const offending = (paths: string[], pattern: RegExp) =>
    paths.flatMap((path) =>
      readFileSync(join(ROOT, path), "utf8")
        .split("\n")
        .flatMap((line, i) => (pattern.test(line) ? [`${path}:${i + 1}: ${line.trim()}`] : [])),
    );

  it("TypeScript in tests/, scripts/ and client/ builds them only through localOnly.ts", () => {
    const sources = ["tests", "scripts", "client"].flatMap((d) => files(d, ".ts")).filter((p) => p !== LOCAL_ONLY);
    assert.isAbove(sources.length, 0, "found no TypeScript sources to check");
    assert.deepEqual(offending(sources, TS_AUTHORITY_CALL), [], "use the builders in tests/utils/localOnly.ts instead");
  });

  it("no shell script calls the CLI's authority commands", () => {
    const scripts = files("scripts", ".sh");
    assert.isAbove(scripts.length, 0, "found no shell scripts to check");
    assert.deepEqual(offending(scripts, SHELL_AUTHORITY_CALL), [], "authority changes on a cluster are made by hand");
  });
});
