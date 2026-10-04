# Integration-test harness for the Solana protocol adapter

The adapter repository gets a Rust crate, `anoma-pa-solana-integration-test`, that lets a test in any repository run a real protocol adapter: start a local Solana runtime with the adapter set up, prove the actions it describes with pa-testkit, settle them, and read the result. It implements pa-testkit's traits, as pa-evm's `anoma-pa-evm-integration-test` does for EVM, and consumers pin it by git tag. Its first consumer is the SPL token forwarder once it moves to anomapay-spl-token-forwarder (anoma/dos-pm#90); it is also the Solana harness of anoma/dos-pm#68 and anoma/pa-testkit#6.

## Where it lives

`crates/integration-test/` at the repository root, the path pa-evm uses. It is its own Cargo workspace with its own `Cargo.lock`. The runtime it drives, surfpool-sdk 1.6.0, pins Solana crates (`solana-instruction ~3.4`, `solana-clock ~3.1`) that, in a shared lockfile, would lower the versions the programs compile against and change their binaries; a separate workspace leaves the programs' lockfile untouched, as `tools/fixture-gen` already does.

Consumers depend on it as the ERC20 forwarder depends on pa-evm's harness:

```toml
anoma-pa-solana-integration-test = { git = "https://github.com/anoma/solana-protocol-adapter", tag = "<tag>" }
```

## The runtime: surfpool

The harness drives surfpool (`surfpool-sdk`): litesvm in-process, served over standard Solana RPC. One RPC code path serves both environments below, as pa-evm's one provider serves anvil and a live chain. Before choosing it, the adapter's real-proof TypeScript suite ran against a surfpool runtime loaded like the local validator: 26 of its 31 spec files passed, including real Groth16 settlement through the devnet verifier stack. The other five failed on the test setup, not the runtime: two upgrade specs write program buffers through the CLI's transaction-port path (the same write over RPC succeeded), the IDL-publishing spec derives a websocket address surfpool does not serve, and two spec files ran against a production binary left in `target/deploy/` (on a development build they pass). The harness applies what that run taught:

- its RPC client uses `confirmed` commitment (surfpool's default block production never finalizes an idle chain);
- it passes the websocket address explicitly (surfpool serves it on a port of its own, which neither web3.js's nor kit's derivation finds);
- it writes program buffers over RPC (surfpool has no transaction port);
- it loads only binaries it ships, never `target/deploy/` (which `verify-build` overwrites with the production build).

## Environments

Two features, as in pa-evm.

**`local`** (default): an offline surfpool runtime producing a block every 400 ms. Setup loads:

- the verifier router from the committed devnet copy (`solana-pa-prototype/devnet-programs/`), with its router PDA;
- the mock verifier, with the VerifierEntry that registers it under selector `0xffffffff`;
- the adapter at its `env/localnet.env` address, with the default signer as upgrade authority;

then initializes the adapter (the default signer as owner, the router, selector `0xffffffff`, the empty kind table) and creates the settlement lookup table. It proves with pa-testkit's `LocalProver`, whose mock seal carries verifier parameters `[u32::MAX; 8]`, that is selector `0xffffffff`.

**`e2e`**: surfpool forking devnet (`remote_rpc_url`, the operator's RPC endpoint from the environment), the adapter at its `env/devnet.env` address with the state devnet holds, and the kind table risc0-kind-tables records for solana-devnet, checked against the one devnet's adapter stores, as pa-evm's e2e checks it. It creates its own settlement lookup table on the fork, the same way `local` does, so it depends on no table record. It proves with pa-testkit's `QueueProver`.

## The protocol adapter

`ProtocolAdapter::execute(transaction)`:

1. checks that every consumed resource's root is one the adapter stores (its current root or a historical-root marker), naming the action and resource that fails;
2. converts the proven transaction into the adapter's settlement input (below);
3. uploads it in chunks to a transaction-data account, settles it as a v0 transaction through the settlement lookup table, and closes the account;
4. adds the created commitments to the host-side commitment tree.

The commitment tree is pa-testkit's `FrontierCommitmentTree`: it starts from the frontier the adapter's `PAState` stores (its commitment count and, per level, the last left node) and adds the leaves settlements create; `root` and `path_to` answer from it. It is pa-evm's tree, which starts from the adapter's stored sides, moved into pa-testkit because nothing in it is chain-specific; only reading the frontier is the harness's.

## Code shared through the client crate

Two pieces of the work exist today outside any library: the conversion of a proven ARM transaction into the adapter's settlement input (in `tools/fixture-gen`), and the assembly of the settlement transactions (in anoma-pa-solana-client's `tools/settle-fixture`). They move into `anoma-pa-solana-client`, behind a feature, as pa-evm's bindings crate holds `conversion.rs`; fixture-gen, `settle-fixture` and the harness all call them:

- **Settlement input.** Re-encode the aggregation proof as the adapter's router `Seal`, then serialize the transaction with bincode. Three receipts occur: a real Groth16 receipt (arm's `encode_seal`); a dev-mode `Fake` receipt (fixture-gen's mock proving); and pa-testkit's `LocalProver` receipt, a Groth16 receipt with verifier parameters `[u32::MAX; 8]`. The two mock receipts become the 260-byte mock seal: selector `0xffffffff`, the receipt's claim digest in `pi_c[..32]`. The conversion re-encodes the receipt as it is, as arm's `encode_seal` does for the EVM adapter; whether the seal proves the transaction's claim is the verifier's to decide, so a tampered seal reaches the adapter and its refusal can be tested.
- **Settlement transactions.** The instructions to create, fill and close the transaction-data account and to settle, the accounts settlement needs (nullifier PDAs, historical-root markers, the new-root marker, each external call's segment), the adapter's `initialize`, and the settlement lookup table's keys.

The client crate builds on `solana-program` 2.1 and `settle-fixture` on Solana 2.2 crates, while the programs (Anchor 1.2.1) and surfpool use the 3.x and 4.x Solana crates, whose types do not interoperate. The client's Solana dependencies move to the versions the programs use.

## The adapter binary the harness loads

A consumer that pins the harness gets its source, not built programs. The harness crate ships the compiled programs it loads, as pa-evm's bindings ship the compiled contract: the adapter's production binary and the mock verifier's binary, committed under `crates/integration-test/programs/`. A check fails when either differs from a fresh deterministic build of the same commit (the executable hash `verify-build` computes), so the committed binaries cannot drift from the source.

## Tests

The harness's own tests, in a CI job of the adapter repository:

- pa-testkit's chain-agnostic suite (`suite`, emitted per environment by `suite_tests!`), which holds pa-evm's integration tests as functions over any `Environment`: a trivial, an n:m, a multi-action and two consume-only transactions settle; the prover refuses invalid witnesses; a transaction whose aggregation seal is tampered with is refused (here with the mock verifier's `ClaimDigestMismatch`);
- after each settlement, the host-side tree's root equals the adapter's (`execute` checks it, so every settling test does);
- the adapter's stored frontier is the sides `FrontierCommitmentTree` starts from, for every count up to 33 leaves;
- a transaction consuming a root the adapter does not store fails before anything is sent, naming the action and resource;
- the settlement-input conversion of each of the three receipts (unit tests in the client crate);
- the committed binaries equal a fresh build;
- the `e2e` tests, which need devnet and the proving queue, run only by an explicit filter.
