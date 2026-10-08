/**
 * dev_set_schema_version and the schema-version guard every instruction
 * that loads pa_state applies.
 */
import * as anchor from "@anchor-lang/core";
import { AccountMeta, PublicKey, Keypair } from "@solana/web3.js";
import { assert } from "chai";
import { denyLogicRefs, migrateState, setKindTableCommitment, upgradeAdapter } from "../client/instructions";
import {
  localCloseMarkersBatch,
  localRenounceAdapterOwnership,
  localTransferAdapterOwnership,
} from "./utils/localOnly";
import { loadFixture } from "./utils/fixtures";
import { randomRef, assertFails } from "./utils/helpers";
import {
  provider,
  program,
  paState,
  DUMMY_ROOT_MARKER,
  deriveNullifierAccounts,
  buildSettleRemainingAccounts,
  setExpiryBounds,
  settleBuilder,
  settleFromTxDataBuilder,
  pauseAsOwner,
  useAdapterSuite,
} from "./utils/adapterSuite";

describe("protocol-adapter (dev_set_schema_version tooling) @localnet", () => {
  const { funder, uploadTxData, initTxData, keepTxData, closeTxData, settleFixtureViaTxData } = useAdapterSuite();

  const setSchemaVersion = (version: number) =>
    program.methods
      .devSetSchemaVersion(version)
      .accountsPartial({ paState, authority: provider.wallet.publicKey })
      .rpc();

  it("dev_set_schema_version rejects a non-authority signer", async () => {
    const intruder = await funder.fresh(1);
    const before = await program.account.paStateAccount.fetch(paState);
    await assertFails(
      program.methods
        .devSetSchemaVersion(before.schemaVersion + 1)
        .accountsPartial({ paState, authority: intruder.publicKey })
        .signers([intruder])
        .rpc(),
      { program, error: "OwnableUnauthorizedAccount" },
    );
    const after = await program.account.paStateAccount.fetch(paState);
    assert.equal(after.schemaVersion, before.schemaVersion, "a rejected call must not change the version");
  });

  it("dev_set_schema_version writes the byte and is reversible", async () => {
    const before = await program.account.paStateAccount.fetch(paState);
    const foreign = before.schemaVersion + 1;
    await setSchemaVersion(foreign);
    const info = await provider.connection.getAccountInfo(paState);
    assert.ok(info, "PAState account should exist");
    assert.equal(info!.data[8], foreign, "the schema version is byte 8 of the account data");

    // The instruction must accept an account of a foreign version, since that
    // is the state a migration instruction starts from.
    await setSchemaVersion(before.schemaVersion);
    const restored = await program.account.paStateAccount.fetch(paState);
    assert.equal(restored.schemaVersion, before.schemaVersion, "version restored");
  });

  describe("schema version guard", () => {
    let current: number;

    // settle_from_txdata and txdata_extend need a TxData account that already
    // exists; txdata_init is itself guarded, so these uploads are created
    // here, before the version flip below.
    let extendAuthority: Keypair;
    let extendUploadId: anchor.BN;
    let extendTxData: PublicKey;
    let settleAuthority: Keypair;
    let settleUploadId: anchor.BN;
    let settleTxData: PublicKey;
    // The two uploads stay open across the cases; after() closes them.
    let keptUploads: ReturnType<typeof keepTxData>[];
    let settleRemainingAccounts: AccountMeta[];

    before(async () => {
      current = (await program.account.paStateAccount.fetch(paState)).schemaVersion;

      extendAuthority = await funder.fresh(2);
      ({ uploadId: extendUploadId, txData: extendTxData } = await initTxData(extendAuthority, 100));

      settleAuthority = await funder.fresh(2);
      const settleFixture = await loadFixture("wrong_root.json");
      const settlePayload = Buffer.from(settleFixture.tx_b64, "base64");
      ({ uploadId: settleUploadId, txData: settleTxData } = await uploadTxData(settleAuthority, settlePayload));
      settleRemainingAccounts = buildSettleRemainingAccounts(
        deriveNullifierAccounts(settleFixture.consumed_nullifiers_b64),
      );

      keptUploads = [keepTxData(extendTxData), keepTxData(settleTxData)];

      await setSchemaVersion(current + 1);
    });

    // txdata_close does not load pa_state, so the uploads close under the
    // foreign version. dev_set_schema_version accepts a foreign version, so
    // the adapter returns to the version it had for every later file.
    after(async () => {
      for (const upload of keptUploads) {
        await closeTxData(upload);
      }
      await setSchemaVersion(current);
    });

    // Each case is an instruction that loads pa_state; with a foreign version
    // byte every one must refuse before doing anything else. pause is
    // last: it would pause the PA for every case after it, and the
    // ownership cases come just before it, since one that went through would
    // take the ownership from every case after it.
    const cases: { name: string; run: () => Promise<unknown> }[] = [
      {
        name: "update_expiry_config",
        run: () => setExpiryBounds(1, 2),
      },
      {
        name: "set_kind_table_commitment",
        run: () => setKindTableCommitment(program, provider.wallet.publicKey, randomRef()).rpc(),
      },
      {
        name: "deny_logic_refs",
        run: () => denyLogicRefs(program, provider.wallet.publicKey, [{ logicRef: randomRef(), consumed: true }]).rpc(),
      },
      {
        name: "settle",
        run: async () => {
          // Tiny payload on purpose: the guard fires during account validation,
          // before the payload is parsed, and a real fixture exceeds the
          // transaction size limit when passed inline.
          const payload = Buffer.from([0, 1, 2, 3]);
          const payer = await funder.fresh(2);
          return settleBuilder(payer.publicKey, payload).signers([payer]).rpc();
        },
      },
      {
        name: "settle_from_txdata",
        run: () =>
          settleFromTxDataBuilder(
            settleAuthority.publicKey,
            settleUploadId,
            settleTxData,
            DUMMY_ROOT_MARKER,
            settleRemainingAccounts,
          )
            .signers([settleAuthority])
            .rpc(),
      },
      {
        name: "txdata_init",
        run: async () => {
          const fx = await loadFixture("wrong_root.json");
          const payload = Buffer.from(fx.tx_b64, "base64");
          const remaining = buildSettleRemainingAccounts(deriveNullifierAccounts(fx.consumed_nullifiers_b64));
          return settleFixtureViaTxData(payload, remaining, { newRootMarker: DUMMY_ROOT_MARKER });
        },
      },
      {
        name: "txdata_extend",
        run: async () => {
          const laterExpiry = new anchor.BN((await provider.connection.getSlot("confirmed")) + 20_000);
          return program.methods
            .txdataExtend(extendUploadId, laterExpiry)
            .accountsStrict({ paState, txData: extendTxData, authority: extendAuthority.publicKey })
            .signers([extendAuthority])
            .rpc();
        },
      },
      {
        name: "close_markers_batch",
        run: () => localCloseMarkersBatch(program, provider.wallet.publicKey, []).rpc(),
      },
      {
        name: "upgrade",
        run: () =>
          upgradeAdapter(
            program,
            provider.wallet.publicKey,
            Keypair.generate().publicKey,
            provider.wallet.publicKey,
          ).rpc(),
      },
      {
        name: "transfer_ownership",
        run: () =>
          localTransferAdapterOwnership(program, provider.wallet.publicKey, Keypair.generate().publicKey).rpc(),
      },
      {
        name: "renounce_ownership",
        run: () => localRenounceAdapterOwnership(program, provider.wallet.publicKey).rpc(),
      },
      {
        name: "pause",
        run: pauseAsOwner,
      },
    ];

    for (const c of cases) {
      it(`${c.name} refuses a foreign schema version`, async () => {
        await assertFails(c.run(), { program, error: "UnsupportedStateSchema" });
      });
    }

    // migrate_state reads only the previous schema version's layout.
    it("migrate_state refuses a version other than the previous one", () =>
      assertFails(migrateState(program, provider.wallet.publicKey).rpc(), { program, error: "NotPreviousSchema" }));
  });
});
