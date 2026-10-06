/**
 * External-call failures during settlement: a wrong CPI account, and the
 * test forwarder's failing and silent modes (localnet only: the test
 * forwarder is deployed nowhere else).
 */
import { Keypair } from "@solana/web3.js";
import { loadFixture } from "./utils/fixtures";
import { assertFails } from "./utils/helpers";
import {
  blockTimeForwarderId,
  blockTimeForwarderProgram,
  program,
  testForwarderProgram,
  testForwarderId,
  DUMMY_ROOT_MARKER,
  deriveNullifierAccounts,
  useAdapterSuite,
} from "./utils/adapterSuite";

describe("protocol-adapter (External call error paths)", () => {
  const { settleFixtureViaTxData } = useAdapterSuite();

  it("rejects settlement when forwarder CPI accounts are wrong", async () => {
    // Use the mismatch fixture (valid proof, nonce=2 nullifiers not consumed).
    // Replace SYSVAR_CLOCK_PUBKEY with a random pubkey so the CPI to btf fails.
    // The inner CPI error propagates through (btf's AccountSysvarMismatch).
    const mismatchFixture = await loadFixture("batch_groth16_mismatch.json");
    const payload = Buffer.from(mismatchFixture.tx_b64, "base64");
    const nullifierAccounts = deriveNullifierAccounts(mismatchFixture.consumed_nullifiers_b64);
    const randomAccount = Keypair.generate().publicKey;
    const remainingAccounts = [
      ...nullifierAccounts,
      { pubkey: blockTimeForwarderId, isWritable: false, isSigner: false },
      { pubkey: randomAccount, isWritable: false, isSigner: false },
    ];

    await assertFails(settleFixtureViaTxData(payload, remainingAccounts, { newRootMarker: DUMMY_ROOT_MARKER }), {
      program: blockTimeForwarderProgram,
      error: "AccountSysvarMismatch",
    });
  });

  it("rejects settlement when test-forwarder returns error @localnet", async () => {
    const failFixture = await loadFixture("batch_forwarder_fail.json");
    const payload = Buffer.from(failFixture.tx_b64, "base64");
    const nullifierAccounts = deriveNullifierAccounts(failFixture.consumed_nullifiers_b64);

    const remainingAccounts = [...nullifierAccounts, { pubkey: testForwarderId, isWritable: false, isSigner: false }];

    // The forwarder's own error fails the settlement.
    await assertFails(settleFixtureViaTxData(payload, remainingAccounts, { newRootMarker: DUMMY_ROOT_MARKER }), {
      program: testForwarderProgram,
      error: "IntentionalFailure",
    });
  });

  it("rejects ForwarderCallOutputMismatch when forwarder returns no data @localnet", async () => {
    const silentFixture = await loadFixture("batch_forwarder_silent.json");
    const payload = Buffer.from(silentFixture.tx_b64, "base64");
    const nullifierAccounts = deriveNullifierAccounts(silentFixture.consumed_nullifiers_b64);

    const remainingAccounts = [...nullifierAccounts, { pubkey: testForwarderId, isWritable: false, isSigner: false }];

    await assertFails(settleFixtureViaTxData(payload, remainingAccounts, { newRootMarker: DUMMY_ROOT_MARKER }), {
      program,
      error: "ForwarderCallOutputMismatch",
    });
  });
});
