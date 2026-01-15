import * as anchor from "@coral-xyz/anchor";
import {
  Connection,
  ComputeBudgetProgram,
  Keypair,
  PublicKey,
  SystemProgram,
  SYSVAR_CLOCK_PUBKEY,
  Transaction,
  VersionedTransaction,
} from "@solana/web3.js";
import { Buffer } from "buffer";

type Fixture = {
  format: string;
  aggregation_strategy: string;
  aggregation_proof_type: string;
  selector: string;
  tx_b64: string;
  tx_tampered_b64: string;
  consumed_nullifiers_b64: string[];
  created_commitments_b64?: string[];
};

type PdaBundle = {
  paState?: PublicKey;
  routerPda?: PublicKey;
  verifierEntryPda?: PublicKey;
  genesisRootMarker?: PublicKey;
  nullifierPdas?: PublicKey[];
};

type PdaExistence = {
  paState?: boolean;
  routerPda?: boolean;
  verifierEntryPda?: boolean;
  genesisRootMarker?: boolean;
};

const PADDING_LEAF = hexToBytes(
  "cc1d2f838445db7aec431df9ee8a871f40e7aa5e064fc056633ef8c60fab7b06"
);

const PA_STATE_SEED = Buffer.from("pa_state");
const NULLIFIER_SEED = Buffer.from("nullifier");
const TX_DATA_SEED = Buffer.from("tx_data");
const ROOT_MARKER_SEED = Buffer.from("root");

class KeypairWallet implements anchor.Wallet {
  payer: Keypair;

  constructor(payer: Keypair) {
    this.payer = payer;
  }

  get publicKey(): PublicKey {
    return this.payer.publicKey;
  }

  async signTransaction<T extends Transaction | VersionedTransaction>(
    tx: T
  ): Promise<T> {
    if ("sign" in tx) {
      tx.sign([this.payer]);
      return tx;
    }
    tx.partialSign(this.payer);
    return tx;
  }

  async signAllTransactions<T extends Transaction | VersionedTransaction>(
    txs: T[]
  ): Promise<T[]> {
    return Promise.all(txs.map((tx) => this.signTransaction(tx)));
  }
}

const state = {
  connection: null as Connection | null,
  wallet: null as KeypairWallet | null,
  provider: null as anchor.AnchorProvider | null,
  program: null as anchor.Program | null,
  idl: null as anchor.Idl | null,
  fixture: null as Fixture | null,
  pdas: {} as PdaBundle,
  pdaExistence: {} as PdaExistence,
  lastUploadId: null as anchor.BN | null,
  lastTxData: null as PublicKey | null,
};

const el = {
  statusOverall: byId("status-overall"),
  rpcUrl: byId<HTMLInputElement>("rpc-url"),
  connectBtn: byId<HTMLButtonElement>("connect-btn"),
  rpcStatus: byId("rpc-status"),
  keypairJson: byId<HTMLTextAreaElement>("keypair-json"),
  loadKeypairBtn: byId<HTMLButtonElement>("load-keypair-btn"),
  airdropBtn: byId<HTMLButtonElement>("airdrop-btn"),
  walletStatus: byId("wallet-status"),
  paProgramId: byId<HTMLInputElement>("pa-program-id"),
  routerProgramId: byId<HTMLInputElement>("router-program-id"),
  groth16ProgramId: byId<HTMLInputElement>("groth16-program-id"),
  forwarderProgramId: byId<HTMLInputElement>("forwarder-program-id"),
  idlFile: byId<HTMLInputElement>("idl-file"),
  idlStatus: byId("idl-status"),
  fixtureFile: byId<HTMLInputElement>("fixture-file"),
  fixtureStatus: byId("fixture-status"),
  payloadSource: byId<HTMLSelectElement>("payload-source"),
  selectorHex: byId<HTMLInputElement>("selector-hex"),
  uploadId: byId<HTMLInputElement>("upload-id"),
  derivePdasBtn: byId<HTMLButtonElement>("derive-pdas-btn"),
  pdaDisplay: byId("pda-display"),
  fetchStateBtn: byId<HTMLButtonElement>("fetch-state-btn"),
  initializeBtn: byId<HTMLButtonElement>("initialize-btn"),
  uploadTxDataBtn: byId<HTMLButtonElement>("upload-txdata-btn"),
  settleBtn: byId<HTMLButtonElement>("settle-btn"),
  extendTxDataBtn: byId<HTMLButtonElement>("extend-txdata-btn"),
  closeTxDataBtn: byId<HTMLButtonElement>("close-txdata-btn"),
  closeExpiredBtn: byId<HTMLButtonElement>("close-expired-btn"),
  emergencyStopBtn: byId<HTMLButtonElement>("emergency-stop-btn"),
  transferAuthorityBtn: byId<HTMLButtonElement>("transfer-authority-btn"),
  updateExpiryBtn: byId<HTMLButtonElement>("update-expiry-btn"),
  expiryOffset: byId<HTMLInputElement>("expiry-offset"),
  chunkSize: byId<HTMLInputElement>("chunk-size"),
  newExpirySlot: byId<HTMLInputElement>("new-expiry-slot"),
  closeExpiredAuthority: byId<HTMLInputElement>("close-expired-authority"),
  newAuthority: byId<HTMLInputElement>("new-authority"),
  minExpiry: byId<HTMLInputElement>("min-expiry"),
  maxExpiry: byId<HTMLInputElement>("max-expiry"),
  extraRootMarkers: byId<HTMLInputElement>("extra-root-markers"),
  newRootHex: byId<HTMLInputElement>("new-root-hex"),
  stateDisplay: byId("state-display"),
  balanceDisplay: byId("balance-display"),
  slotDisplay: byId("slot-display"),
  logOutput: byId("log-output"),
};

const readiness = {
  rpc: false,
  wallet: false,
  idl: false,
  program: false,
  pda: false,
  fixture: false,
};

function byId<T extends HTMLElement = HTMLElement>(id: string): T {
  const node = document.getElementById(id);
  if (!node) {
    throw new Error(`Missing element: ${id}`);
  }
  return node as T;
}

function setReady(key: keyof typeof readiness, value: boolean) {
  readiness[key] = value;
  const item = document.querySelector(`[data-status="${key}"]`);
  if (item) {
    item.classList.toggle("ready", value);
  }
  updateOverallStatus();
}

function updateOverallStatus() {
  const readyCount = Object.values(readiness).filter(Boolean).length;
  if (readyCount === Object.keys(readiness).length) {
    el.statusOverall.textContent = "All systems ready";
    return;
  }
  el.statusOverall.textContent = `${readyCount} / ${
    Object.keys(readiness).length
  } ready`;
}

function log(message: string) {
  const timestamp = new Date().toLocaleTimeString();
  el.logOutput.textContent = `[${timestamp}] ${message}\n${el.logOutput.textContent}`;
}

function logError(err: unknown) {
  const lines = extractErrorLines(err);
  if (lines.length === 0) {
    log("Unexpected error");
    return;
  }
  for (const line of lines) {
    log(`Error: ${line}`);
  }
}

function extractErrorLines(err: unknown): string[] {
  if (!err) return [];
  const anyErr = err as any;
  const lines: string[] = [];
  if (typeof anyErr === "string") lines.push(anyErr);
  if (anyErr.message) lines.push(anyErr.message);
  if (anyErr.error?.errorMessage) lines.push(anyErr.error.errorMessage);
  if (Array.isArray(anyErr.logs)) lines.push(...anyErr.logs);
  if (Array.isArray(anyErr.error?.logs)) lines.push(...anyErr.error.logs);
  return Array.from(new Set(lines.filter(Boolean)));
}

async function logTransaction(label: string, signature: string) {
  log(`${label}: ${signature}`);
  if (!state.connection) return;
  try {
    const tx = await state.connection.getTransaction(signature, {
      maxSupportedTransactionVersion: 0,
      commitment: "confirmed",
    });
    if (tx?.meta?.logMessages?.length) {
      log(`Logs for ${label}:\n${tx.meta.logMessages.join("\n")}`);
    }
  } catch (err) {
    log(`Unable to fetch logs for ${label}`);
    logError(err);
  }
}

function parsePublicKey(value: string, label: string): PublicKey {
  if (!value) {
    throw new Error(`${label} is required`);
  }
  return new PublicKey(value.trim());
}

function hexToBytes(hex: string): Uint8Array {
  const cleaned = hex.replace(/^0x/, "");
  if (cleaned.length % 2 !== 0) {
    throw new Error("Hex string must have even length");
  }
  const out = new Uint8Array(cleaned.length / 2);
  for (let i = 0; i < cleaned.length; i += 2) {
    out[i / 2] = Number.parseInt(cleaned.slice(i, i + 2), 16);
  }
  return out;
}

function base64ToBytes(b64: string): Uint8Array {
  const bin = atob(b64);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i += 1) {
    out[i] = bin.charCodeAt(i);
  }
  return out;
}

function u64ToLeBytes(value: bigint): Uint8Array {
  const out = new Uint8Array(8);
  let temp = value;
  for (let i = 0; i < 8; i += 1) {
    out[i] = Number(temp & 0xffn);
    temp >>= 8n;
  }
  return out;
}

function parseSelector(selectorHex: string): Uint8Array {
  const bytes = hexToBytes(selectorHex);
  if (bytes.length !== 4) {
    throw new Error("Selector must be 4 bytes (8 hex chars)");
  }
  return bytes;
}

function ensureProgram(): anchor.Program {
  if (!state.program) {
    throw new Error("Program not ready. Load IDL + wallet + connection.");
  }
  return state.program;
}

function ensureConnection(): Connection {
  if (!state.connection) {
    throw new Error("RPC not connected");
  }
  return state.connection;
}

function ensureWallet(): KeypairWallet {
  if (!state.wallet) {
    throw new Error("Wallet not loaded");
  }
  return state.wallet;
}

function updatePdaDisplay() {
  const { paState, routerPda, verifierEntryPda, genesisRootMarker } = state.pdas;
  const nullifierCount = state.pdas.nullifierPdas?.length ?? 0;
  const status = state.pdaExistence;
  el.pdaDisplay.innerHTML = `
    <div>paState: ${paState?.toBase58() ?? "-"} ${
      status.paState === undefined ? "" : status.paState ? "(exists)" : "(missing)"
    }</div>
    <div>router PDA: ${routerPda?.toBase58() ?? "-"} ${
      status.routerPda === undefined ? "" : status.routerPda ? "(exists)" : "(missing)"
    }</div>
    <div>verifier entry: ${verifierEntryPda?.toBase58() ?? "-"} ${
      status.verifierEntryPda === undefined
        ? ""
        : status.verifierEntryPda
        ? "(exists)"
        : "(missing)"
    }</div>
    <div>genesis marker: ${genesisRootMarker?.toBase58() ?? "-"} ${
      status.genesisRootMarker === undefined
        ? ""
        : status.genesisRootMarker
        ? "(exists)"
        : "(missing)"
    }</div>
    <div>nullifiers: ${nullifierCount}</div>
  `;
}

async function checkPdaExistence() {
  const connection = state.connection;
  if (!connection) return;
  const { paState, routerPda, verifierEntryPda, genesisRootMarker } = state.pdas;
  const [paInfo, routerInfo, verifierInfo, genesisInfo] = await Promise.all([
    paState ? connection.getAccountInfo(paState) : Promise.resolve(null),
    routerPda ? connection.getAccountInfo(routerPda) : Promise.resolve(null),
    verifierEntryPda
      ? connection.getAccountInfo(verifierEntryPda)
      : Promise.resolve(null),
    genesisRootMarker
      ? connection.getAccountInfo(genesisRootMarker)
      : Promise.resolve(null),
  ]);
  state.pdaExistence = {
    paState: Boolean(paInfo),
    routerPda: Boolean(routerInfo),
    verifierEntryPda: Boolean(verifierInfo),
    genesisRootMarker: Boolean(genesisInfo),
  };
  updatePdaDisplay();
}

async function refreshBalanceAndSlot() {
  const connection = state.connection;
  const wallet = state.wallet;
  if (!connection || !wallet) return;
  const [balance, slot] = await Promise.all([
    connection.getBalance(wallet.publicKey),
    connection.getSlot("confirmed"),
  ]);
  el.balanceDisplay.textContent = `${(balance / anchor.web3.LAMPORTS_PER_SOL).toFixed(3)} SOL`;
  el.slotDisplay.textContent = `${slot}`;
}

async function setupProgramIfPossible() {
  if (!state.connection || !state.wallet || !state.idl) return;
  try {
    const programId = parsePublicKey(el.paProgramId.value, "PA Program ID");
    state.provider = new anchor.AnchorProvider(state.connection, state.wallet, {
      commitment: "confirmed",
    });
    state.program = new anchor.Program(state.idl, programId, state.provider);
    setReady("program", true);
  } catch (err) {
    setReady("program", false);
    logError(err);
  }
}

async function connectRpc() {
  const url = el.rpcUrl.value.trim();
  const connection = new Connection(url, "confirmed");
  try {
    const version = await connection.getVersion();
    state.connection = connection;
    el.rpcStatus.textContent = `Connected (solana-core ${version["solana-core"]})`;
    setReady("rpc", true);
    log(`Connected to ${url}`);
  } catch (err) {
    state.connection = null;
    setReady("rpc", false);
    el.rpcStatus.textContent = "Connection failed";
    logError(err);
    return;
  }
  await setupProgramIfPossible();
  await refreshBalanceAndSlot();
}

async function loadKeypair() {
  const raw = el.keypairJson.value.trim();
  if (!raw) {
    throw new Error("Paste a keypair JSON array.");
  }
  const parsed = JSON.parse(raw) as number[];
  const payer = Keypair.fromSecretKey(new Uint8Array(parsed));
  state.wallet = new KeypairWallet(payer);
  el.walletStatus.textContent = `Wallet: ${payer.publicKey.toBase58()}`;
  setReady("wallet", true);
  await setupProgramIfPossible();
  await refreshBalanceAndSlot();
}

async function requestAirdrop() {
  const connection = ensureConnection();
  const wallet = ensureWallet();
  const sig = await connection.requestAirdrop(
    wallet.publicKey,
    2 * anchor.web3.LAMPORTS_PER_SOL
  );
  await connection.confirmTransaction(sig, "confirmed");
  await refreshBalanceAndSlot();
  await logTransaction("Airdrop", sig);
}

async function loadIdlFromFile(file: File) {
  const text = await file.text();
  state.idl = JSON.parse(text) as anchor.Idl;
  el.idlStatus.textContent = `IDL loaded (${state.idl.name})`;
  setReady("idl", true);
  await setupProgramIfPossible();
}

async function loadFixtureFromFile(file: File) {
  const text = await file.text();
  state.fixture = JSON.parse(text) as Fixture;
  el.fixtureStatus.textContent = `Fixture loaded (${state.fixture.aggregation_proof_type})`;
  setReady("fixture", true);
  if (state.fixture.selector) {
    el.selectorHex.value = state.fixture.selector;
  }
}

async function derivePdas() {
  const program = ensureProgram();
  const fixture = state.fixture;
  const programId = program.programId;
  const paState = PublicKey.findProgramAddressSync([PA_STATE_SEED], programId)[0];
  const selectorSource = el.selectorHex.value.trim() || fixture?.selector;
  if (!selectorSource) {
    throw new Error("Selector not set");
  }
  const selector = parseSelector(selectorSource);

  const routerProgram = parsePublicKey(
    el.routerProgramId.value,
    "Verifier Router Program ID"
  );
  const routerPda = PublicKey.findProgramAddressSync(
    [Buffer.from("router")],
    routerProgram
  )[0];
  const verifierEntryPda = PublicKey.findProgramAddressSync(
    [Buffer.from("verifier"), Buffer.from(selector)],
    routerProgram
  )[0];
  const genesisRootMarker = PublicKey.findProgramAddressSync(
    [ROOT_MARKER_SEED, paState.toBuffer(), Buffer.from(PADDING_LEAF)],
    programId
  )[0];
  const nullifierPdas =
    fixture?.consumed_nullifiers_b64.map((nfB64) => {
      const nf = base64ToBytes(nfB64);
      return PublicKey.findProgramAddressSync(
        [NULLIFIER_SEED, paState.toBuffer(), Buffer.from(nf)],
        programId
      )[0];
    }) ?? [];

  state.pdas = {
    paState,
    routerPda,
    verifierEntryPda,
    genesisRootMarker,
    nullifierPdas,
  };
  setReady("pda", true);
  updatePdaDisplay();
  await checkPdaExistence();
  log("Derived PDAs.");
}

async function fetchState() {
  const program = ensureProgram();
  const paState = state.pdas.paState;
  if (!paState) throw new Error("PDAs not derived");
  const account = await program.account.paStateAccount.fetch(paState);
  const rootBytes = Buffer.from(account.root as number[]);
  el.stateDisplay.innerHTML = `
    <div>authority: ${account.authority.toBase58()}</div>
    <div>paused: ${account.paused}</div>
    <div>root: ${rootBytes.toString("hex")}</div>
    <div>next_index: ${account.nextIndex.toString()}</div>
    <div>depth: ${account.currentDepth}</div>
    <div>min_expiry: ${account.minExpirySlots?.toString?.() ?? "-"}</div>
    <div>max_expiry: ${account.maxExpirySlots?.toString?.() ?? "-"}</div>
  `;
  await refreshBalanceAndSlot();
  log("Fetched PA state.");
}

async function initializePa() {
  const program = ensureProgram();
  const wallet = ensureWallet();
  const paState = state.pdas.paState;
  const genesisRootMarker = state.pdas.genesisRootMarker;
  if (!paState || !genesisRootMarker) {
    throw new Error("PDAs not derived");
  }
  const sig = await program.methods
    .initialize()
    .accounts({
      paState,
      payer: wallet.publicKey,
      systemProgram: SystemProgram.programId,
    })
    .remainingAccounts([
      { pubkey: genesisRootMarker, isWritable: true, isSigner: false },
    ])
    .rpc();
  await logTransaction("Initialize", sig);
  await checkPdaExistence();
  await fetchState();
}

function resolveUploadId(): anchor.BN {
  const input = el.uploadId.value.trim();
  const value = input ? BigInt(input) : BigInt(Date.now());
  const bn = new anchor.BN(value.toString());
  state.lastUploadId = bn;
  el.uploadId.value = bn.toString();
  return bn;
}

function getUploadId(): anchor.BN {
  const input = el.uploadId.value.trim();
  if (input) {
    const bn = new anchor.BN(input);
    state.lastUploadId = bn;
    return bn;
  }
  if (!state.lastUploadId) {
    throw new Error("Upload ID not set. Upload TxData or enter an Upload ID.");
  }
  return state.lastUploadId;
}

function deriveTxDataPda(
  programId: PublicKey,
  authority: PublicKey,
  uploadId: anchor.BN
): PublicKey {
  const idBytes = u64ToLeBytes(BigInt(uploadId.toString()));
  return PublicKey.findProgramAddressSync(
    [TX_DATA_SEED, authority.toBuffer(), Buffer.from(idBytes)],
    programId
  )[0];
}

async function uploadTxData() {
  const program = ensureProgram();
  const connection = ensureConnection();
  const wallet = ensureWallet();
  const paState = state.pdas.paState;
  if (!paState) throw new Error("PDAs not derived");
  if (!state.fixture) throw new Error("Fixture not loaded");

  const source = el.payloadSource.value as keyof Fixture;
  const payloadB64 = state.fixture[source];
  if (!payloadB64) throw new Error(`Fixture missing ${source}`);
  const payload = base64ToBytes(payloadB64);
  const uploadId = resolveUploadId();
  const txData = deriveTxDataPda(program.programId, wallet.publicKey, uploadId);
  state.lastTxData = txData;

  const expiryOffset = Number.parseInt(el.expiryOffset.value, 10);
  const chunkSize = Number.parseInt(el.chunkSize.value, 10);
  if (!Number.isFinite(expiryOffset) || expiryOffset <= 0) {
    throw new Error("Expiry offset must be a positive number");
  }
  if (!Number.isFinite(chunkSize) || chunkSize <= 0) {
    throw new Error("Chunk size must be a positive number");
  }
  const slot = await connection.getSlot("confirmed");
  const expiresSlot = new anchor.BN(slot + expiryOffset);

  const initSig = await program.methods
    .txdataInit(uploadId, payload.length, expiresSlot)
    .accounts({
      paState,
      txData,
      authority: wallet.publicKey,
      systemProgram: SystemProgram.programId,
    })
    .signers([wallet.payer])
    .rpc();
  await logTransaction("TxData init", initSig);

  for (let offset = 0; offset < payload.length; offset += chunkSize) {
    const chunk = payload.slice(offset, Math.min(payload.length, offset + chunkSize));
    const sig = await program.methods
      .txdataWrite(uploadId, offset, Buffer.from(chunk))
      .accounts({
        txData,
        authority: wallet.publicKey,
      })
      .signers([wallet.payer])
      .rpc();
    log(`TxData write ${offset}-${offset + chunk.length}: ${sig}`);
  }
  await refreshBalanceAndSlot();
  log("TxData upload complete.");
}

async function settleFromTxData() {
  const program = ensureProgram();
  const wallet = ensureWallet();
  const paState = state.pdas.paState;
  if (!paState) throw new Error("PDAs not derived");
  if (!state.fixture) throw new Error("Fixture not loaded");
  const uploadId = getUploadId();
  const txData = deriveTxDataPda(program.programId, wallet.publicKey, uploadId);
  state.lastTxData = txData;

  const routerProgram = parsePublicKey(
    el.routerProgramId.value,
    "Verifier Router Program ID"
  );
  const groth16Program = parsePublicKey(
    el.groth16ProgramId.value,
    "Groth16 Verifier Program ID"
  );
  const selectorSource = el.selectorHex.value.trim() || state.fixture.selector;
  if (!selectorSource) throw new Error("Selector not set");
  const selector = parseSelector(selectorSource);

  const routerPda = state.pdas.routerPda ??
    PublicKey.findProgramAddressSync([Buffer.from("router")], routerProgram)[0];
  const verifierEntryPda = state.pdas.verifierEntryPda ??
    PublicKey.findProgramAddressSync(
      [Buffer.from("verifier"), Buffer.from(selector)],
      routerProgram
    )[0];

  const nullifierPdas = state.pdas.nullifierPdas ?? [];
  const remainingAccounts = nullifierPdas.map((pubkey) => ({
    pubkey,
    isWritable: true,
    isSigner: false,
  }));

  const forwarderId = parsePublicKey(
    el.forwarderProgramId.value,
    "Block Time Forwarder ID"
  );
  remainingAccounts.push(
    { pubkey: forwarderId, isWritable: false, isSigner: false },
    { pubkey: SYSVAR_CLOCK_PUBKEY, isWritable: false, isSigner: false }
  );

  const extraMarkers = el.extraRootMarkers.value
    .split(",")
    .map((entry) => entry.trim())
    .filter(Boolean);
  for (const marker of extraMarkers) {
    remainingAccounts.push({
      pubkey: new PublicKey(marker),
      isWritable: false,
      isSigner: false,
    });
  }

  const newRootHex = el.newRootHex.value.trim();
  if (newRootHex) {
    const rootBytes = hexToBytes(newRootHex);
    if (rootBytes.length !== 32) {
      throw new Error("New root hex must be 32 bytes");
    }
    const newRootMarker = PublicKey.findProgramAddressSync(
      [ROOT_MARKER_SEED, paState.toBuffer(), Buffer.from(rootBytes)],
      program.programId
    )[0];
    remainingAccounts.push({
      pubkey: newRootMarker,
      isWritable: true,
      isSigner: false,
    });
  }

  const sig = await program.methods
    .settleFromTxdata(uploadId)
    .accounts({
      paState,
      txData,
      authority: wallet.publicKey,
      systemProgram: SystemProgram.programId,
      verifierRouterProgram: routerProgram,
      router: routerPda,
      verifierEntry: verifierEntryPda,
      verifierProgram: groth16Program,
    })
    .remainingAccounts(remainingAccounts)
    .preInstructions([
      ComputeBudgetProgram.setComputeUnitLimit({ units: 1_400_000 }),
    ])
    .signers([wallet.payer])
    .rpc();
  await logTransaction("Settle", sig);
  await fetchState();
}

async function extendTxData() {
  const program = ensureProgram();
  const wallet = ensureWallet();
  const paState = state.pdas.paState;
  if (!paState) throw new Error("PDAs not derived");
  const uploadId = getUploadId();
  const txData = deriveTxDataPda(program.programId, wallet.publicKey, uploadId);
  state.lastTxData = txData;
  const input = el.newExpirySlot.value.trim();
  const connection = ensureConnection();
  const slot = await connection.getSlot("confirmed");
  const newExpiry = input ? new anchor.BN(input) : new anchor.BN(slot + 5000);

  const sig = await program.methods
    .txdataExtend(uploadId, newExpiry)
    .accountsStrict({
      paState,
      txData,
      authority: wallet.publicKey,
    })
    .signers([wallet.payer])
    .rpc();
  await logTransaction("TxData extend", sig);
}

async function closeTxData() {
  const program = ensureProgram();
  const wallet = ensureWallet();
  const uploadId = getUploadId();
  const txData = deriveTxDataPda(program.programId, wallet.publicKey, uploadId);
  state.lastTxData = txData;
  const sig = await program.methods
    .txdataClose(uploadId)
    .accounts({
      txData,
      authority: wallet.publicKey,
      refund: wallet.publicKey,
    })
    .signers([wallet.payer])
    .rpc();
  await logTransaction("TxData close", sig);
}

async function closeExpiredTxData() {
  const program = ensureProgram();
  const wallet = ensureWallet();
  const uploadId = getUploadId();
  const authorityKey = el.closeExpiredAuthority.value.trim();
  if (!authorityKey) {
    throw new Error("Close expired authority is required");
  }
  const authority = new PublicKey(authorityKey);
  const txData = deriveTxDataPda(program.programId, authority, uploadId);
  const sig = await program.methods
    .txdataCloseExpired(uploadId, authority)
    .accountsStrict({
      txData,
      payer: wallet.publicKey,
      refund: authority,
    })
    .signers([wallet.payer])
    .rpc();
  await logTransaction("TxData close expired", sig);
}

async function emergencyStop() {
  const program = ensureProgram();
  const wallet = ensureWallet();
  const paState = state.pdas.paState;
  if (!paState) throw new Error("PDAs not derived");
  const sig = await program.methods
    .emergencyStop()
    .accounts({
      paState,
      authority: wallet.publicKey,
    })
    .signers([wallet.payer])
    .rpc();
  await logTransaction("Emergency stop", sig);
}

async function transferAuthority() {
  const program = ensureProgram();
  const wallet = ensureWallet();
  const paState = state.pdas.paState;
  if (!paState) throw new Error("PDAs not derived");
  const newAuthRaw = el.newAuthority.value.trim();
  if (!newAuthRaw) throw new Error("New authority is required");
  const newAuthority = new PublicKey(newAuthRaw);
  const sig = await program.methods
    .transferAuthority(newAuthority)
    .accounts({
      paState,
      authority: wallet.publicKey,
    })
    .signers([wallet.payer])
    .rpc();
  await logTransaction("Transfer authority", sig);
}

async function updateExpiryConfig() {
  const program = ensureProgram();
  const wallet = ensureWallet();
  const paState = state.pdas.paState;
  if (!paState) throw new Error("PDAs not derived");
  const min = Number.parseInt(el.minExpiry.value, 10);
  const max = Number.parseInt(el.maxExpiry.value, 10);
  if (!Number.isFinite(min) || !Number.isFinite(max)) {
    throw new Error("Min and max expiry slots must be numbers");
  }
  const sig = await program.methods
    .updateExpiryConfig(new anchor.BN(min), new anchor.BN(max))
    .accounts({
      paState,
      authority: wallet.publicKey,
    })
    .signers([wallet.payer])
    .rpc();
  await logTransaction("Update expiry config", sig);
}

function bindEvents() {
  el.connectBtn.addEventListener("click", () => runAction("Connect", connectRpc));
  el.loadKeypairBtn.addEventListener("click", () => runAction("Load keypair", loadKeypair));
  el.airdropBtn.addEventListener("click", () => runAction("Airdrop", requestAirdrop));
  el.idlFile.addEventListener("change", async (event) => {
    const file = (event.target as HTMLInputElement).files?.[0];
    if (file) {
      await runAction("Load IDL", () => loadIdlFromFile(file));
    }
  });
  el.fixtureFile.addEventListener("change", async (event) => {
    const file = (event.target as HTMLInputElement).files?.[0];
    if (file) {
      await runAction("Load fixture", () => loadFixtureFromFile(file));
    }
  });
  el.derivePdasBtn.addEventListener("click", () => runAction("Derive PDAs", derivePdas));
  el.fetchStateBtn.addEventListener("click", () => runAction("Fetch state", fetchState));
  el.initializeBtn.addEventListener("click", () => runAction("Initialize", initializePa));
  el.uploadTxDataBtn.addEventListener("click", () => runAction("Upload TxData", uploadTxData));
  el.settleBtn.addEventListener("click", () => runAction("Settle", settleFromTxData));
  el.extendTxDataBtn.addEventListener("click", () => runAction("Extend TxData", extendTxData));
  el.closeTxDataBtn.addEventListener("click", () => runAction("Close TxData", closeTxData));
  el.closeExpiredBtn.addEventListener("click", () => runAction("Close expired TxData", closeExpiredTxData));
  el.emergencyStopBtn.addEventListener("click", () => runAction("Emergency stop", emergencyStop));
  el.transferAuthorityBtn.addEventListener("click", () => runAction("Transfer authority", transferAuthority));
  el.updateExpiryBtn.addEventListener("click", () => runAction("Update expiry config", updateExpiryConfig));
}

async function runAction(label: string, action: () => Promise<void>) {
  try {
    log(`→ ${label}`);
    await action();
    await refreshBalanceAndSlot();
    log(`✓ ${label} complete`);
  } catch (err) {
    logError(err);
  }
}

bindEvents();
updateOverallStatus();
