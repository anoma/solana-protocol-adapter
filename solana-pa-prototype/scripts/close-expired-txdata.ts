/**
 * Close expired TxData accounts owned by the PA program.
 *
 * Usage:
 *   ANCHOR_PROVIDER_URL=https://api.mainnet-beta.solana.com \
 *   ANCHOR_WALLET=/path/to/fee-payer.json \
 *   npx ts-node -P tsconfig.json scripts/close-expired-txdata.ts [--dry-run]
 *
 * Rent is returned by the PA program to each TxData account's recorded refund
 * address. The caller only pays transaction fees.
 */
import * as anchor from "@coral-xyz/anchor";
import { Program } from "@coral-xyz/anchor";
import { LAMPORTS_PER_SOL, PublicKey, Transaction } from "@solana/web3.js";
import bs58 from "bs58";
import { SolanaPaPrototype } from "../target/types/solana_pa_prototype";

type TxDataSummary = {
  pubkey: PublicKey;
  refund: PublicKey;
  authority: PublicKey;
  expiresSlot: bigint;
  writtenLen: number;
  lamports: number;
};

const TX_DATA_DISCRIMINATOR = Buffer.from([
  80, 208, 0, 159, 216, 183, 139, 138,
]);
const BATCH_SIZE = 12;

function readTxData(
  pubkey: PublicKey,
  account: { data: Buffer; lamports: number }
): TxDataSummary {
  const data = account.data;
  if (data.length < 85) {
    throw new Error(
      `TxData account ${pubkey.toBase58()} is too short: ${data.length} bytes`
    );
  }

  return {
    pubkey,
    authority: new PublicKey(data.subarray(9, 41)),
    refund: new PublicKey(data.subarray(41, 73)),
    writtenLen: data.readUInt32LE(73),
    expiresSlot: data.readBigUInt64LE(77),
    lamports: account.lamports,
  };
}

async function main() {
  const args = process.argv.slice(2);
  const unknownArgs = args.filter((arg) => arg !== "--dry-run");
  if (unknownArgs.length > 0) {
    console.error(
      `Unknown argument(s): ${unknownArgs.join(
        ", "
      )}. Usage: close-expired-txdata.ts [--dry-run]`
    );
    process.exit(1);
  }
  const dryRun = args.includes("--dry-run");

  const provider = anchor.AnchorProvider.env();
  anchor.setProvider(provider);

  const program = anchor.workspace
    .SolanaPaPrototype as Program<SolanaPaPrototype>;
  const connection = provider.connection;
  const wallet = provider.wallet as anchor.Wallet;

  console.log("Program: ", program.programId.toBase58());
  console.log("Payer:   ", wallet.publicKey.toBase58());

  const currentSlot = BigInt(await connection.getSlot("finalized"));
  console.log("Finalized slot:", currentSlot.toString());

  const accounts = await connection.getProgramAccounts(program.programId, {
    filters: [
      {
        memcmp: {
          offset: 0,
          bytes: bs58.encode(TX_DATA_DISCRIMINATOR),
        },
      },
    ],
  });

  const txdata = accounts.map(({ pubkey, account }) =>
    readTxData(pubkey, {
      data: account.data,
      lamports: account.lamports,
    })
  );
  const expired = txdata
    .filter((account) => currentSlot > account.expiresSlot)
    .sort((a, b) => (a.expiresSlot < b.expiresSlot ? -1 : 1));

  const openRent = txdata.reduce((sum, account) => sum + account.lamports, 0);
  const expiredRent = expired.reduce(
    (sum, account) => sum + account.lamports,
    0
  );

  console.log(`Found ${txdata.length} TxData accounts`);
  console.log(`Expired: ${expired.length}`);
  console.log(`Open rent: ${(openRent / LAMPORTS_PER_SOL).toFixed(9)} SOL`);
  console.log(
    `Expired rent: ${(expiredRent / LAMPORTS_PER_SOL).toFixed(9)} SOL`
  );

  if (expired.length === 0) {
    return;
  }

  for (const account of expired) {
    console.log(
      `  ${account.pubkey.toBase58()} expires=${account.expiresSlot} ` +
        `written=${account.writtenLen} refund=${account.refund.toBase58()} ` +
        `rent=${(account.lamports / LAMPORTS_PER_SOL).toFixed(9)} SOL`
    );
  }

  if (dryRun) {
    console.log("Dry run only; no accounts closed.");
    return;
  }

  let closed = 0;
  let failed = 0;
  let recovered = 0;

  for (let i = 0; i < expired.length; i += BATCH_SIZE) {
    const batch = expired.slice(i, i + BATCH_SIZE);
    const transaction = new Transaction();

    for (const account of batch) {
      const ix = await program.methods
        .txdataCloseExpired()
        .accounts({
          txData: account.pubkey,
          payer: wallet.publicKey,
          refund: account.refund,
        })
        .instruction();
      transaction.add(ix);
    }

    try {
      const signature = await provider.sendAndConfirm(transaction, [], {
        commitment: "confirmed",
      });
      closed += batch.length;
      recovered += batch.reduce((sum, account) => sum + account.lamports, 0);
      console.log(
        `Closed batch ${Math.floor(i / BATCH_SIZE) + 1}: ${batch.length} ` +
          `accounts, signature ${signature}`
      );
    } catch (e: any) {
      failed += batch.length;
      console.error(
        `Failed batch ${Math.floor(i / BATCH_SIZE) + 1} ` +
          `(${batch.length} accounts): ${e.message}`
      );
    }
  }

  console.log(
    `Closed ${closed}/${expired.length} expired TxData accounts; ` +
      `${failed} failed; rent returned ${(recovered / LAMPORTS_PER_SOL).toFixed(
        9
      )} SOL`
  );

  if (failed > 0) {
    process.exit(1);
  }
}

main().catch((err) => {
  console.error("Close expired TxData failed:", err);
  process.exit(1);
});
