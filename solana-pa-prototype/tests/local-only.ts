/**
 * Authority changes and account closures on a live cluster are made by hand,
 * never by repository code: the instructions that move or renounce an upgrade
 * authority, name the forwarder's emergency caller, or close escrows, the
 * forwarder config, nonce bitmaps or adapter markers are built only by
 * tests/utils/localOnly.ts, which refuses any RPC endpoint that is not this
 * machine's. On 2026-10-01 a test pointed at devnet renounced the devnet
 * adapter's upgrade authority.
 */
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
  localSetEmergencyCaller,
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

  it("builds every one against the local validator", () => {
    for (const [name, build] of Object.entries(builders(program, forwarderProgram))) {
      assert.isOk(build(), name);
    }
  });
});
