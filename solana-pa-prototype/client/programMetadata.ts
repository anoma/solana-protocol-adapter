/**
 * A program's canonical IDL account in the Program Metadata program (the
 * account explorers and `anchor idl fetch` read), written through the
 * Program Metadata library. Only the program's upgrade authority writes it,
 * so once `initialize` hands that authority to the program, the account keeps
 * the IDL published at deployment.
 */
import { readFileSync } from "fs";
import { address, createClient } from "@solana/kit";
import { solanaRpc } from "@solana/kit-plugin-rpc";
import { signerFromFile } from "@solana/kit-plugin-signer";
import {
  fetchMetadataContent,
  findCanonicalPda,
  Format,
  getProgramAuthority,
  packDirectData,
  programMetadataProgram,
} from "@solana-program/program-metadata";

/**
 * Write the IDL JSON at `idlPath` to its program's canonical IDL account,
 * signing and paying with the keypair file `walletPath`, the program's
 * upgrade authority, over `rpcUrl` (kit's RPC confirms at `confirmed`). Any
 * other signer is refused before a transaction is sent. The cluster must then
 * serve exactly that document.
 */
export async function publishIdl(rpcUrl: string, walletPath: string, idlPath: string): Promise<void> {
  const content = readFileSync(idlPath, "utf8");
  const program = address(JSON.parse(content).address as string);
  const client = await createClient()
    .use(signerFromFile(walletPath))
    .use(solanaRpc({ rpcUrl }))
    .use(programMetadataProgram());
  const wallet = client.identity.address;
  const [[metadata], { authority: upgradeAuthority, programData }] = await Promise.all([
    findCanonicalPda({ program, seed: "idl" }),
    getProgramAuthority(client.rpc, program),
  ]);
  if (upgradeAuthority !== wallet) {
    throw new Error(
      `${wallet} cannot write ${program}'s canonical IDL account ${metadata}: ` +
        `the program's upgrade authority is ${upgradeAuthority ?? "none"}`,
    );
  }

  await client.programMetadata.writeMetadata({
    authority: client.identity,
    program,
    seed: "idl",
    metadata,
    programData,
    format: Format.Json,
    ...packDirectData({ content }),
  });

  // Read back what the cluster now serves rather than assuming the write landed.
  if ((await fetchMetadataContent(client.rpc, program, "idl")) !== content) {
    throw new Error(`${program}'s canonical IDL account ${metadata} does not serve ${idlPath} after the write`);
  }
}
