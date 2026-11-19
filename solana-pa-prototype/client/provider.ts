/**
 * The provider every operator script and spec file uses: the endpoint and
 * wallet of the Anchor environment (ANCHOR_PROVIDER_URL, ANCHOR_WALLET), with
 * every transaction confirmed, and every read made, at `confirmed`. A cluster's
 * RPC endpoint answers from several nodes; at Anchor's default, `processed`
 * (one node's unvoted view), a blockhash or a write one node has seen may be
 * unknown to the node that answers next, and a transaction then fails its
 * simulation with "Blockhash not found".
 */
import { AnchorProvider } from "@anchor-lang/core";
import { Connection } from "@solana/web3.js";

export function confirmedProvider(): AnchorProvider {
  const env = AnchorProvider.env();
  return new AnchorProvider(new Connection(env.connection.rpcEndpoint, "confirmed"), env.wallet, {
    commitment: "confirmed",
    preflightCommitment: "confirmed",
  });
}
