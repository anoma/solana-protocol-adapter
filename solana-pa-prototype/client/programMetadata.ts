/**
 * A program's canonical IDL account in the Program Metadata program (the
 * account explorers and `anchor idl fetch` read), written through the
 * program's own client library rather than its CLI.
 *
 * The program lets two signers write a canonical metadata account: the
 * program's upgrade authority, and the explicit authority set on the account
 * (`program-metadata set-authority`). The CLI admits only the first, so once a
 * program's upgrade authority is its own PDA (`initialize`), the account's
 * explicit authority, the owner, could no longer update it through the CLI.
 */
import { readFileSync } from "fs";
import { type Address, address, createClient, isSome } from "@solana/kit";
import { solanaRpc } from "@solana/kit-plugin-rpc";
import { signerFromFile } from "@solana/kit-plugin-signer";
import {
  fetchMaybeMetadata,
  fetchMetadataContent,
  findCanonicalPda,
  Format,
  getProgramAuthority,
  packDirectData,
  programMetadataProgram,
} from "@solana-program/program-metadata";

/** How the IDL account is written: as the program's upgrade authority, or as the account's explicit authority. */
export type IdlWriter = "upgrade authority" | "metadata authority";

/**
 * A Program Metadata client over `rpcUrl`, signing and paying with the
 * keypair file `walletPath`. Kit's RPC confirms at `confirmed`.
 */
export function programMetadataClient(rpcUrl: string, walletPath: string) {
  return createClient().use(signerFromFile(walletPath)).use(solanaRpc({ rpcUrl })).use(programMetadataProgram());
}

/** `program`'s canonical IDL account and its ProgramData account. */
export async function canonicalIdlAccount(client: Awaited<ReturnType<typeof programMetadataClient>>, program: Address) {
  const [[metadata], { authority: upgradeAuthority, programData }] = await Promise.all([
    findCanonicalPda({ program, seed: "idl" }),
    getProgramAuthority(client.rpc, program),
  ]);
  return { metadata, upgradeAuthority, programData };
}

/**
 * Write the IDL JSON at `idlPath` to its program's canonical IDL account,
 * signing and paying with the keypair file `walletPath` over `rpcUrl`. The
 * upgrade authority creates the account or updates it; once it exists, its
 * explicit authority updates it too. Any other signer is refused before a
 * transaction is sent. The cluster must then serve exactly that document.
 */
export async function publishIdl(rpcUrl: string, walletPath: string, idlPath: string): Promise<IdlWriter> {
  const content = readFileSync(idlPath, "utf8");
  const program = address(JSON.parse(content).address as string);
  const client = await programMetadataClient(rpcUrl, walletPath);
  const wallet = client.identity.address;
  const { metadata, upgradeAuthority, programData } = await canonicalIdlAccount(client, program);
  const account = await fetchMaybeMetadata(client.rpc, metadata);
  const metadataAuthority: Address | undefined =
    account.exists && isSome(account.data.authority) ? account.data.authority.value : undefined;

  let writer: IdlWriter;
  if (upgradeAuthority === wallet) {
    writer = "upgrade authority";
  } else if (metadataAuthority === wallet) {
    writer = "metadata authority";
  } else {
    throw new Error(
      `${wallet} can write neither ${program}'s canonical IDL account ${metadata}: ` +
        `the program's upgrade authority is ${upgradeAuthority ?? "none"}, ` +
        (account.exists
          ? `the account's authority ${metadataAuthority ?? "none"}`
          : "and the account does not exist yet"),
    );
  }

  await client.programMetadata.writeMetadata({
    authority: client.identity,
    program,
    seed: "idl",
    metadata,
    // The upgrade authority proves itself through the program's ProgramData;
    // the explicit authority is checked against the account alone.
    programData: writer === "upgrade authority" ? programData : undefined,
    format: Format.Json,
    ...packDirectData({ content }),
  });

  // Read back what the cluster now serves rather than assuming the write
  // landed. The document need not keep the file's key order or whitespace.
  const served = await fetchMetadataContent(client.rpc, program, "idl");
  if (canonicalJson(served) !== canonicalJson(content)) {
    throw new Error(`${program}'s canonical IDL account ${metadata} does not serve ${idlPath} after the write`);
  }
  return writer;
}

/** `json` with every object's keys sorted, so two encodings of one document compare equal. */
function canonicalJson(json: string): string {
  const canon = (v: unknown): unknown =>
    Array.isArray(v)
      ? v.map(canon)
      : v !== null && typeof v === "object"
        ? Object.fromEntries(
            Object.keys(v)
              .sort()
              .map((k) => [k, canon((v as Record<string, unknown>)[k])]),
          )
        : v;
  return JSON.stringify(canon(JSON.parse(json)));
}
