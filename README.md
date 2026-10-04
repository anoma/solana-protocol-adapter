# Solana Protocol Adapter

Port of the [EVM Protocol Adapter V2](https://github.com/anoma/pa-evm/tree/next) to Solana, using the RISC0 proving backend.

## Repository Structure

```
solana-protocol-adapter/
├── crates/integration-test/ # the integration-test harness other repositories test against
└── solana-pa-prototype/     # Solana PA implementation
    ├── programs/            # the adapter, its example and test forwarders, and the mock verifier
    ├── client/              # instruction builders shared by the operator scripts and the tests
    ├── scripts/             # dev.sh, ops.sh and the operator scripts
    ├── tests/               # the integration suite (one validator)
    ├── tools/fixture-gen/   # proves the test fixtures
    └── docs/                # OPERATIONS.md (runbook), INTEGRATION.md (clients and indexers)
```

The RISC0 verifier programs are not built here: the test tooling copies the deployed verifier router, the Groth16 verifier and their state accounts from devnet.

## Local Development Setup

### Prerequisites

- Nix with flakes enabled (`nix --version`)
- Supported host platforms for the pinned Agave toolchain: `x86_64-linux`, `x86_64-darwin`, `aarch64-darwin`

### Quick Start

```bash
# Clone the repository
git clone https://github.com/anoma/solana-protocol-adapter
cd solana-protocol-adapter/solana-pa-prototype

# Build and run the integration suite (dev.sh enters the pinned Nix shell itself)
./scripts/dev.sh anchor-test
```

The script:
1. Builds the programs at the addresses in `env/localnet.env`.
2. Starts one validator with the programs, the devnet verifier stack and Program Metadata program (the committed copy in `devnet-programs/`) and the suite's settlement lookup table loaded at genesis, warped to slot 1, and runs every spec file under `tests/` against it, so the deployment builds up as much history as the suite makes: first `tests/fresh/` (initialization, which only works on a fresh deployment), then every other file, each building on whatever state it finds, then `tests/terminal/` in numbered order (denials, marker teardown, the renounced ownership: changes no later test could run after).

### Proof Modes

The integration suite runs in one of two proof modes:

- **real** (default): fixtures carry Groth16 aggregation proofs (selector
  `0x73c457ba`), verified on-chain by the devnet-copied RISC0 Groth16
  verifier. Regenerating them requires full proving (hours of CPU for the
  set, and a container runtime): `./scripts/dev.sh regen-fixtures real`.
- **mock** (`./scripts/dev.sh anchor-test --mode mock`, or
  `PA_TEST_MODE=mock`): fixtures carry mock seals (selector `0xffffffff`)
  accepted only by the localnet-only
  `mock-verifier` program, which the validator's synthetic `VerifierEntry`
  account registers in the RISC0 router at genesis. The transactions are
  byte-identical to the real fixtures except the seal; generation executes
  the circuits without proving and takes seconds:
  `./scripts/dev.sh regen-fixtures mock`.

The adapter program is identical in both modes: each deployment pins one
verifier selector at `initialize`, so a deployment pinned to the Groth16
selector never accepts mock seals. `regen-fixtures` regenerates the complete
set for one mode sequentially (`scripts/regen-fixtures.sh` is the single copy
of the recipe); `./scripts/dev.sh gen-fixtures <shape> [options] OUT` is the
entry point for one fixture.

### Available Commands

| Command | Description |
|---------|-------------|
| `./scripts/dev.sh anchor-test [--mode mock] [spec file...]` | Build the programs and run the integration suite on one validator; spec files restrict the run |
| `./scripts/dev.sh test` | Run the Rust unit tests |
| `./scripts/dev.sh fmt` / `clippy` | Format check / lints with CI's flags |
| `./scripts/dev.sh anchor-build` | Development build of the programs (dev-teardown enabled) |
| `./scripts/dev.sh release-build` | Production build (verifies dev-only instructions are absent) |
| `./scripts/dev.sh validator` | Start a local validator with the devnet programs (verifier stack, Program Metadata), without the workspace programs |
| `./scripts/dev.sh validator-deploy` | Build, start a validator with every program loaded at genesis, and keep it running for external clients |
| `./scripts/dev.sh gen-fixtures` / `regen-fixtures` / `fixture-test` | Fixture generation and fixture-gen's tests |
| `./scripts/dev.sh harness-test [--e2e]` | The integration-test harness's tests on surfpool; with `--e2e`, its e2e cases on a fork of devnet (`DEVNET_RPC_URL`), proven by the queue (`QUEUE_BASE_URL`, `QUEUE_AUTH_TOKEN`) |
| `./scripts/dev.sh harness-programs [--check]` | Write the harness's program binaries (the deterministic builds); `--check` fails when the committed ones are not |
| `./scripts/dev.sh lock-check` / `lock-sync <pkg>` | Check / re-align the Cargo.lock files' shared dependencies |
| `./scripts/dev.sh update-deps` | Regenerate `yarn.lock` |
| `./scripts/dev.sh coverage` | Unit-test line coverage |
| `./scripts/dev.sh clean` | Remove local validator/test artifacts |
| `./scripts/dev.sh shell` / `run <cmd>` | Interactive Nix shell / one command in it |
| `./scripts/dev.sh <op> --cluster <c>` | Cluster operations (`deploy`, `upgrade`, `init`, `set-kind-table`, `deny-logic-ref`, `lookup-table`, `pause`, `unpause`, `status`, `balance`, `idl-publish`, `verify-build`) against `localnet`/`devnet`/`mainnet`: see `scripts/ops.sh` for flags and `docs/OPERATIONS.md` for procedures |

### Rebuilding From Scratch

```bash
./scripts/dev.sh clean
./scripts/dev.sh anchor-test
```

### Updating Toolchain Inputs

To update pinned Nix dependencies:

```bash
nix --extra-experimental-features 'nix-command flakes' flake update
```

### Interactive Development

```bash
./scripts/dev.sh shell
```

Inside the shell, from `solana-pa-prototype/`:
```bash
# Build programs (SBPF v3 with the flake's platform-tools; see scripts/validator-deploy.sh)
./scripts/ops.sh build-dev

# Run one spec file on a fresh validator (it initializes what it needs)
./scripts/ops.sh test tests/settle.ts
```

---

## Writing a Forwarder

A forwarder is a Solana program the adapter calls by CPI while it settles a transaction: for each resource, in pa-evm's order, the adapter makes the calls that resource's `app_data` carries, before it verifies the transaction's proof (a later verification failure reverts everything). Forwarders execute side effects (token transfers, state updates, etc.) and return output that the adapter compares with the output the proof commits to.

### Forwarder Interface

Your forwarder must implement a `forward_call` instruction:

```rust
use anchor_lang::prelude::*;
use anchor_lang::solana_program::program::set_return_data;

#[program]
pub mod my_forwarder {
    use super::*;

    pub fn forward_call(
        ctx: Context<ForwardCall>,
        logic_ref: [u8; 32],  // The calling resource's logic ref
        input: Vec<u8>,       // Arbitrary input from the RM transaction
    ) -> Result<()> {
        // Your logic here...

        // Return output via set_return_data (max 1024 bytes)
        let output: Vec<u8> = /* your output */;
        set_return_data(&output);

        Ok(())
    }
}

#[derive(Accounts)]
pub struct ForwardCall<'info> {
    // Your required accounts...
}
```

### Key Requirements

1. **Instruction discriminator**: The adapter calls using Anchor's discriminator: `sha256("global:forward_call")[..8]` = `0x9faae00afd696cde`

2. **Instruction data format**:
   ```
   [discriminator: 8 bytes]
   [logic_ref: 32 bytes]
   [input_len: 4 bytes, little-endian u32]
   [input: N bytes]
   ```

3. **Output via return data**: Use `set_return_data(&output)`. The adapter reads it immediately after the CPI and requires it to equal the call's `expected_output`, which the resource's `app_data.external_payload` carries inside the transaction's proven `AggregationInstance`. Solana caps return data at 1024 bytes: a forwarder that needs to surface more must commit a digest in return data and place the full payload elsewhere (e.g. an event or PDA).

---

## Tutorial: BlockTimeForwarder Walkthrough

This tutorial walks through the adapter's external call mechanism using the `block-time-forwarder` example program.

### What BlockTimeForwarder Does

The forwarder compares an expected timestamp (embedded in the RM transaction) against Solana's current clock time:

| Return Value | Meaning |
|--------------|---------|
| `0` | expected < current (LT) |
| `1` | expected == current (EQ) |
| `2` | expected > current (GT) |

The fixture uses timestamp `-1` (before Unix epoch), so against any current time it returns `0` (LT).

### Step 1: Run the Tests

```bash
cd solana-pa-prototype
./scripts/dev.sh anchor-test tests/settle.ts
```

The test **"accepts a valid Groth16 batch aggregation tx and creates root marker"** settles `batch_groth16.json`, whose one created resource carries a block-time-forwarder call. The forwarder logs nothing itself; the adapter emits `ForwarderCallExecutedEvent { forwarder, input, output }` for the call, with `output = [0]`, and fails the settlement with `ExternalCallOutputMismatch` if the returned byte differs from the proof's (the test "reverts on unexpected forwarder call output" settles `batch_groth16_mismatch.json` to show it).

### Step 2: See Verification Fail

The test **"rejects a tampered tx (proof binding)"** settles the fixture with one byte of its transaction changed. The Groth16 proof commits to the journal of the transaction's `AggregationInstance`; changing the transaction changes that journal, so the verifier rejects the proof: the Groth16 verifier with `VerificationError` (6000), or, in mock mode, the mock verifier with `ClaimDigestMismatch` (6600).

### Step 3: Understand the Flow

A settlement (`settle`, or `settle_from_txdata` after a TxData upload) runs in pa-evm's order:

1. **Checks** — the transaction carries an aggregation and a delta proof, the compliance key and kind table match, no resource carries a denied logic ref, no nullifier repeats, and every consumed root is a known root.
2. **Per action**, first its consumed resources (create each nullifier's marker PDA, which refuses a spent nullifier; run the resource's forwarder calls; emit its payload events), then its created resources (append each commitment to the tree; run its calls; emit its events), then `ActionExecutedEvent`.
3. **Proof verification** — a CPI to the RISC0 verifier router, which routes the seal to the verifier registered for its selector; then the delta proof.
4. **Root** — if the transaction created commitments, the new root's marker PDA is recorded and `CommitmentTreeRootAddedEvent` emitted.
5. `TransactionExecutedEvent`.

### How the Forwarder Works

The forwarder implements a single instruction, `forward_call`:

```rust
pub fn forward_call(
    ctx: Context<ForwardCall>,
    _logic_ref: [u8; 32],  // The calling resource's logic ref (unused by this forwarder)
    input: Vec<u8>,        // 8 bytes: expected timestamp as i64 LE
) -> Result<()> {
    if input.len() != 8 {
        return Err(ErrorCode::InvalidInput.into());
    }
    let expected_time = i64::from_le_bytes(input.try_into().map_err(|_| ErrorCode::InvalidInput)?);
    let current_time = ctx.accounts.clock.unix_timestamp;

    let result = match expected_time.cmp(&current_time) {
        Ordering::Less => RESULT_LT,
        Ordering::Greater => RESULT_GT,
        Ordering::Equal => RESULT_EQ,
    };

    set_return_data(&[result]);
    Ok(())
}
```

The adapter's CPI passes the resource's `logic_ref` and the call's `input`. After the call, it reads the return data and compares it with the call's `expected_output`.

### How External Calls Are Encoded

Each external call is one entry in a resource's `app_data.external_payload`, inside the transaction's `AggregationInstance`:

```rust
pub struct SolanaExternalCall {
    pub program_id: [u8; 32],       // Forwarder program ID
    pub instruction_data: Vec<u8>,  // Passed as `input` to forward_call
    pub expected_output: Vec<u8>,   // Must match return data; must be non-empty
    pub output_mode: OutputMode,    // ReturnData (only variant)
    pub num_accounts: u8,           // Accounts in this call's segment, including the forwarder
}
```

The fixture generator (`tools/fixture-gen`) encodes this structure with the forwarder's program ID, a timestamp, and the expected comparison result. The byte-level format is in `docs/INTEGRATION.md`, "External call encoding".

### Assumption Boundary (EVM Parity)

The Solana adapter mirrors pa-evm's trust model:

1. Aggregation proof verification is bound to the journal re-derived from the transaction's `AggregationInstance` via `AggregationInstance::to_journal()` (`anoma-rm-core`), matching pa-evm V2's `Aggregation.toJournal`.
2. External call execution and output checks read the same instance's per-resource `app_data.external_payload`.
3. Because (1) and (2) read the same instance, any change to a call flows into the aggregation journal and invalidates the proof: no duplicated wire field can diverge.

### How the Test Passes Accounts

The test provides the forwarder and its dependencies via `remaining_accounts`:

```typescript
const allRemainingAccounts = [
  ...nullifierPdas,  // Writable PDAs for nullifier marking
  { pubkey: blockTimeForwarderId, isWritable: false, isSigner: false },
  { pubkey: SYSVAR_CLOCK_PUBKEY, isWritable: false, isSigner: false },
];
```

After the nullifier markers, each external call takes the next `num_accounts` accounts, in the order the calls run.

### Create Your Own Forwarder

1. **Scaffold the program**: create `programs/my-forwarder/` (`anchor new my-forwarder` from `solana-pa-prototype/`).

2. **Implement `forward_call`** in `programs/my-forwarder/src/lib.rs`:
   ```rust
   use anchor_lang::prelude::*;
   use anchor_lang::solana_program::program::set_return_data;

   // The address comes from env/<cluster>.env (env/README.md).
   declare_id!(Pubkey::from_str_const(env!("MY_FORWARDER_PROGRAM_ID")));

   #[program]
   pub mod my_forwarder {
       use super::*;

       pub fn forward_call(
           ctx: Context<ForwardCall>,
           logic_ref: [u8; 32],
           input: Vec<u8>,
       ) -> Result<()> {
           // Your logic here
           let output = vec![/* result bytes */];
           set_return_data(&output);
           Ok(())
       }
   }

   #[derive(Accounts)]
   pub struct ForwardCall<'info> {
       // Accounts your forwarder needs
   }
   ```

3. **Register and build**: add a row to `PROGRAM_TABLE` in `scripts/validator-deploy.sh` (every program under `programs/` needs one, or builds fail), give the program an address in `env/localnet.env` (`MY_FORWARDER_PROGRAM_ID=<any public key>`: the local validator loads it at genesis there, so no keypair is needed), then run `./scripts/dev.sh anchor-build`.

4. **Update `fixture-gen`** to encode a `SolanaExternalCall` with your forwarder's program ID, input, expected output and account count.

5. **Update tests** to include your forwarder's program and required accounts in `remaining_accounts`.

---

## Testing Against the Adapter From Another Repository

`crates/integration-test` (`anoma-pa-solana-integration-test`) implements [pa-testkit](https://github.com/anoma/pa-testkit)'s `Environment` for this adapter, as pa-evm's `anoma-pa-evm-integration-test` does for EVM. A test proves actions with pa-testkit and settles them on a real adapter:

```toml
anoma-pa-solana-integration-test = { git = "https://github.com/anoma/solana-protocol-adapter", tag = "<tag>" }
```

```rust
use anoma_pa_solana_integration_test::envs::local::Environment;

let mut env = Environment::setup_bare().await?;
let tx = anoma_pa_testkit::prove_actions(&env, &actions).await?;
anoma_pa_testkit::execute_tx(&mut env, tx).await?;
```

- **`local`** (default feature): an offline [surfpool](https://github.com/txtx/surfpool) runtime with the verifier router and Program Metadata program copies, the mock verifier, and the adapter build the crate ships (`crates/integration-test/programs/`, the deterministic build of the tag, which CI checks), initialized with the mock selector; pa-testkit's local prover.
- **`e2e`**: a runtime forking devnet (`DEVNET_RPC_URL`), on the adapter devnet runs with the state it holds, the kind table recorded for devnet checked against the one it stores; pa-testkit's queue prover (`QUEUE_BASE_URL`, `QUEUE_AUTH_TOKEN`).

`ProtocolAdapter::execute` settles the way every submitter does (the client crate's `settlement_input` and `plan_settlement`: upload, settle as a v0 transaction through a settlement lookup table, close), and the commitment tree pa-testkit builds from the adapter's frontier must give the root the adapter stores after each settlement. A transaction whose proof calls a forwarder settles once the forwarder is registered: the proof commits each call but not its accounts, so `env.protocol_adapter.forwarders.register(program, forwarder)` names, for that program, a `forwarders::Forwarder` that gives each call its CPI segment and any instructions that must precede the settlement (a wrap's ed25519 signature check). A call to an unregistered program fails before anything is sent. `extend_lookup_table` adds a forwarder's fixed accounts to the settlement lookup table, as a deployment's table holds them. A consumer deploys its own program with `env.deploy_program` and sends its setup (a mint, a token account, an approval) with `env.send`, which the default signer pays for. `env.protocol_adapter.settle` settles as `execute` does and returns the settlement's signature, and `executed::Executed::read` reads a confirmed transaction back: its log and the events each program emitted by self-invocation. The local environment's `deploy_test_forwarder` deploys the adapter's test forwarder, whose log mode fills a transaction's log budget, and `deploy_block_time_forwarder` its example block-time forwarder, which the local environment calls as a `suite::BlockTimeForwarder`. `env.write_buffer` places a program in a loader buffer, the code an upgrade instruction installs.

The test state holds, as pa-evm's harness keeps them, the cluster and the runtime's RPC endpoint (`state::cluster`), the funded default signer (`state::actors`) and the adapter's address (`state::pa`); `suite_tests!` emits pa-testkit's chain-agnostic tests for an environment, including an external call to the block-time forwarder settled and refused.

## Building a Client

Clients submit RM transactions to the adapter for settlement. `docs/INTEGRATION.md` is the full contract; `client/` holds the builders the operator scripts and tests use.

### Transaction Flow

1. **Upload the RM transaction** when it does not fit in one Solana transaction alongside the settle accounts (the 1232-byte limit covers the whole transaction):
   ```typescript
   // Initialize TxData account
   await program.methods.txdataInit(uploadId, capacity, expiresSlot)
     .accounts({ txData, authority, systemProgram })
     .rpc();

   // Write chunks
   for (let offset = 0; offset < payload.length; offset += chunkSize) {
     await program.methods.txdataWrite(uploadId, offset, payload.subarray(offset, offset + chunkSize))
       .accounts({ txData, authority })
       .rpc();
   }
   ```

2. **Settle**:
   ```typescript
   await program.methods.settleFromTxdata(uploadId)
     .accounts({
       paState,
       txData,
       authority,
       systemProgram,
       newRootMarker,       // the post-settlement root's marker PDA; null when the transaction creates nothing
       verifierRouterProgram,
       router: routerPda,
       verifierEntry: verifierEntryPda,
       verifierProgram: groth16VerifierId,
     })
     .remainingAccounts([
       // Nullifier marker PDAs (one per consumed resource)
       ...nullifierPdas.map(pubkey => ({ pubkey, isWritable: true, isSigner: false })),
       // External call segments (forwarder program + its required accounts)
       { pubkey: forwarderProgramId, isWritable: false, isSigner: false },
       { pubkey: forwarderAccount1, isWritable: false, isSigner: false },
       // ... more forwarder accounts
     ])
     .preInstructions([
       ComputeBudgetProgram.setComputeUnitLimit({ units: 1_400_000 }),
       ComputeBudgetProgram.requestHeapFrame({ bytes: 256 * 1024 }),
     ])
     .rpc();
   ```

### Deriving PDAs

```typescript
const PA_STATE_SEED = Buffer.from("pa_state");
const NULLIFIER_SEED = Buffer.from("nullifier");
const TX_DATA_SEED = Buffer.from("tx_data");
const ROOT_MARKER_SEED = Buffer.from("root");

// PA state (singleton)
const [paState] = PublicKey.findProgramAddressSync([PA_STATE_SEED], programId);

// Nullifier marker (one per consumed nullifier)
const [nullifierPda] = PublicKey.findProgramAddressSync(
  [NULLIFIER_SEED, paState.toBuffer(), nullifierBytes],
  programId
);

// TxData account
const uploadIdLe = Buffer.alloc(8);
uploadIdLe.writeBigUInt64LE(BigInt(uploadId));
const [txData] = PublicKey.findProgramAddressSync(
  [TX_DATA_SEED, authority.toBuffer(), uploadIdLe],
  programId
);

// Root marker
const [rootMarker] = PublicKey.findProgramAddressSync(
  [ROOT_MARKER_SEED, paState.toBuffer(), rootBytes],
  programId
);
```

### remaining_accounts Layout

The adapter expects `remaining_accounts` in this order:

1. **Nullifier PDAs** (N accounts, writable) - One per consumed resource, in instance order
2. **External call segments** - One per call, in the order the calls run; each is the forwarder program account followed by the accounts that forwarder needs, `num_accounts` in all
3. **Historical root markers** (optional, read-only) - For transactions anchored to historical roots. Position-independent: validity is checked by scanning the whole list.

The **new root marker PDA** is not part of `remaining_accounts`: it is the
optional named `new_root_marker` account of `settle`/`settle_from_txdata`
(writable). A settlement that creates commitments must pass the marker PDA of
the post-settlement root; one that creates nothing produces no root and must
omit it. Anything else fails with `RootPdaMismatch`.

---

## Fixtures

Fixtures contain pre-generated RM transactions with valid proofs, committed because real proving takes hours of CPU for the full set. The suite runs on one deployment, so each fixture has one role: settled by exactly one test, settled by whichever test first needs a settled fixture to resubmit (`batch_groth16_resubmitted.json`), or never settled (`batch_groth16_rejected.json` and its error variants, the deliberate-failure forwarder fixtures). A spend through a Merkle path (the historical-root consumer) depends on the tree the deployment holds when the spent resource settles, so it is a fresh-phase test: `regen-fixtures.sh` proves it over the tree `tests/fresh/` builds in its fixed order (`fixture-gen ... --settled-before FIXTURE`, once per earlier settlement), and the test checks that its fixture's root is the one the deployment will hold before settling anything.

### Fixture Format

```json
{
  "name": "batch_groth16",
  "format": "arm-risc0:Transaction(bincode)",
  "aggregation_strategy": "batch",
  "aggregation_proof_type": "groth16",
  "selector": "0x73c457ba",
  "tx_b64": "<base64 encoded RM transaction>",
  "tx_tampered_b64": "<base64 encoded tampered transaction>",
  "consumed_nullifiers_b64": ["<base64>", ...],
  "created_commitments_b64": ["<base64>", ...],
  "historical_roots_b64": ["<base64>", ...]
}
```

`name` is what the fixture's resource nonces derive from: its file stem, followed by `/<salt>` when generated with `--salt`. `historical_roots_b64` is present only for fixtures anchored to a historical root.

### Generating Fixtures

`./scripts/dev.sh regen-fixtures <real|mock> [--out DIR] [--salt SALT] [--kind-table PATH]` regenerates the whole set (a salt sets every nonce apart from earlier runs, so the set settles on a deployment that already holds another run's); `./scripts/dev.sh gen-fixtures <shape> [options] OUT` generates one (`gen-fixtures --help` lists the shapes). Proving runs locally by default; its Groth16 step needs a container runtime (the Nix shell provides podman behind a `docker` wrapper). Setting `QUEUE_BASE_URL` and `QUEUE_AUTH_TOKEN`, or passing `--prover queue`, sends the proving jobs to the AnomaPay workers queue instead.

The fixtures' external calls ride on pa-testkit's pass-through logic (`anoma_pa_testkit::fixtures::passthrough`), whose guest pa-testkit ships prebuilt.

### Fixture Staleness

Fixtures embed program IDs (the block-time and test forwarders') and depend on the guest image IDs. If either changes, the fixtures must be regenerated; `anchor-test` checks that the primary fixture names the current block-time forwarder.

---

## Deployment

### Local Testing

The local validator and the tests never touch the network. `devnet-programs/` holds a committed copy of the RISC0 verifier router, the Groth16 verifier, their state accounts and the Program Metadata program as they are on devnet (`DEVNET_CLONE_PROGRAMS`, `DEVNET_CLONE_ACCOUNTS` in `scripts/validator-deploy.sh`), and `start_validator` loads them at genesis. `./scripts/dev.sh refresh-devnet-programs --url <devnet rpc>` replaces the copy with devnet's current state:

| Address | Account |
|---|---|
| `BetEAE4npinksQBxvqUN1KkCVjYFJywWao45MSWtp5yg` | Verifier router program |
| `2Yfa83Lzbn71ie3J1KQRiNQz1qHnvVm8gkBCpXZQ7ajD` | Groth16 verifier program |
| `9ZJmYSYaYq38GfwQMsEw5gkzfr94Vbzw6Nv53yQuCv2S` | Router PDA (initialized state) |
| `4ktbrXwBXZMoND5qb3J6abS1m8KqwUtCjjDBebJ4vqey` | Verifier entry PDA (Groth16 selector registered) |

### A Local Validator With the Programs

```bash
cd solana-pa-prototype
./scripts/dev.sh validator-deploy                       # every program loaded at genesis, kept running
# or, on a running local validator:
./scripts/dev.sh deploy all --cluster localnet           # deploys and initializes (see OPERATIONS.md for the PA_* / STF_* variables)
```

### Deploy to a Real Cluster

Deployment, initialization, upgrades, pausing, and retirement procedures for
devnet/mainnet live in
[`solana-pa-prototype/docs/OPERATIONS.md`](solana-pa-prototype/docs/OPERATIONS.md).

---

## Troubleshooting

### `nix develop` Fails

Ensure flakes are enabled:
```bash
nix --extra-experimental-features 'nix-command flakes' develop
```

### Unsupported Host: `aarch64-linux`

The pinned Agave `v4.3.0` release does not publish `aarch64-unknown-linux-gnu` binaries.
Use an `x86_64-linux` host (or macOS) for this repo's pinned Nix workflow.

### Validator Won't Start / Connection Refused

The validator may have crashed or port 8899 is in use:
```bash
# Check if port is in use
lsof -i :8899

# Clean up and retry
./scripts/dev.sh clean
./scripts/dev.sh anchor-test
```

### DeclaredProgramIdMismatch (Error 4100)

A program runs at another address than the one compiled into it: its binary in `target/deploy/` was built from another cluster's `env/<cluster>.env`, or before an address there changed. Rebuild it:
```bash
./scripts/dev.sh anchor-build
```

### Fixture Generation Fails

`fixture-gen` needs a container runtime for local Groth16 proving. Check it is available and that `fixture-gen` builds and starts:
```bash
./scripts/dev.sh run docker --version
./scripts/dev.sh run cargo build --locked --manifest-path tools/fixture-gen/Cargo.toml
./scripts/dev.sh run tools/fixture-gen/target/debug/fixture-gen --help
```

### Slow First Build

The first build downloads and compiles many dependencies. Subsequent builds reuse the local Nix/cargo caches and are much faster.
