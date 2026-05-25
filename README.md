# Solana Protocol Adapter

Port of the [EVM Protocol Adapter](https://github.com/anoma/evm-protocol-adapter) to Solana, using RISC0 proving backend.

## Repository Structure

```
solana-protocol-adapter/
└── solana-pa-prototype/     # Solana PA implementation
```

The RISC0 Groth16 verifier programs are automatically cloned from devnet during testing (no local build required).

## Local Development Setup

### Prerequisites

- Nix with flakes enabled (`nix --version`)
- Supported host platforms for the pinned Agave toolchain: `x86_64-linux`, `x86_64-darwin`, `aarch64-darwin`

### Quick Start

```bash
# Clone the repository
git clone https://github.com/anoma/solana-protocol-adapter
cd solana-protocol-adapter

# Enter the pinned development environment
nix --extra-experimental-features 'nix-command flakes' develop

# Build and run tests
cd solana-pa-prototype
./scripts/dev.sh anchor-test
```

That's it! The script will:
1. Ensure toolchain and dependencies are available in the Nix shell
2. Build the Solana programs
3. Start a local validator with RISC0 verifier programs cloned from devnet
4. Run the full test suite

### Available Commands

| Command | Description |
|---------|-------------|
| `./scripts/dev.sh anchor-test` | Build programs and run integration tests |
| `./scripts/dev.sh shell` | Open an interactive shell in the Nix dev environment |
| `./scripts/dev.sh anchor-build` | Build Anchor programs only |
| `./scripts/dev.sh test` | Run Rust unit tests |
| `./scripts/dev.sh validator` | Start the local validator |
| `./scripts/dev.sh clean` | Remove local validator/test artifacts |

### Rebuilding From Scratch

If you encounter issues, do a complete rebuild:

```bash
# Remove local validator/test state
./scripts/dev.sh clean

# Run tests
./scripts/dev.sh anchor-test
```

### Updating Toolchain Inputs

To update pinned Nix dependencies:

```bash
nix --extra-experimental-features 'nix-command flakes' flake update
```

### Interactive Development

For iterative development, use the shell command:

```bash
./scripts/dev.sh shell
```

Inside the shell:
```bash
cd solana-pa-prototype

# Build programs
anchor build

# Run tests (start validator separately first)
yarn run ts-mocha -p ./tsconfig.json -t 1000000 'tests/**/*.ts'
```

To start the validator in another terminal:
```bash
cd solana-pa-prototype
./scripts/dev.sh validator
```

---

## Writing a Forwarder

A forwarder is a Solana program that the PA calls via CPI after proof verification. Forwarders execute side effects (token transfers, state updates, etc.) and return output that the PA verifies against the proof.

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
        logic_ref: [u8; 32],  // Resource's logic verifying key
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

1. **Instruction discriminator**: The PA calls using Anchor's discriminator: `sha256("global:forward_call")[..8]` = `0x9faae00afd696cde`

2. **Instruction data format**:
   ```
   [discriminator: 8 bytes]
   [logic_ref: 32 bytes]
   [input_len: 4 bytes, little-endian u32]
   [input: N bytes]
   ```

3. **Output via return data**: Use `set_return_data(&output)`. The PA reads this immediately after CPI and verifies it matches `expected_output` from `LogicVerifierInputs.app_data.external_payload`. Solana caps return data at 1024 bytes — forwarders that need to surface more must commit a digest in return data and place the full payload elsewhere (e.g. an event or PDA).

---

## Tutorial: BlockTimeForwarder Walkthrough

This tutorial demonstrates the PA's external call mechanism using the `block-time-forwarder` example program. You'll run tests, observe the CPI flow in logs, see what happens when verification fails, and then create your own forwarder.

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
./scripts/dev.sh anchor-test
```

Find the test **"accepts a valid Groth16 batch aggregation tx"** and look for these log lines:

```
BlockTimeForwarder: forward_call invoked
  logic_ref: [...]
  expected_time: -1
  current_time: [current unix timestamp]
  result: LT (expected < current)
  return_data set: [0]
```

This shows:
1. The PA called the forwarder via CPI
2. The forwarder decoded `-1` from the 8-byte input
3. It compared against the current clock
4. It returned `0` via `set_return_data`
5. The PA verified this matched `expected_output` from the proof

### Step 2: See Verification Fail

Find the test **"rejects a tampered tx"**. The logs show:

```
Proof verification failed
```

Why it fails:
- The Groth16 proof commits to a specific journal digest
- Tampering the transaction changes the digest
- The verifier rejects the mismatched proof

### Step 3: Understand the Flow

The full settlement sequence visible in the passing test logs:

1. **TxData upload** — `txdataInit` allocates account, `txdataWrite` streams chunks
2. **Proof verification** — CPI to `groth_16_verifier`, journal digest computed from transaction bytes
3. **External call** — CPI to `block-time-forwarder`, return data captured and compared
4. **Commitment update** — new commitment appended to the on-chain tree
5. **Nullifier marking** — PDA created for each consumed nullifier (prevents double-spend)

### How the Forwarder Works

The forwarder implements a single instruction, `forward_call`:

```rust
pub fn forward_call(
    ctx: Context<ForwardCall>,
    logic_ref: [u8; 32],  // Resource's logic key (for access control)
    input: Vec<u8>,       // 8 bytes: expected timestamp as i64 LE
) -> Result<()> {
    let expected_time = i64::from_le_bytes(input.try_into()?);
    let current_time = ctx.accounts.clock.unix_timestamp;

    let result = match expected_time.cmp(&current_time) {
        Ordering::Less => 0,
        Ordering::Equal => 1,
        Ordering::Greater => 2,
    };

    set_return_data(&[result]);
    Ok(())
}
```

The PA's CPI call passes `logic_ref` and `input` extracted from the RM transaction. After the call, it reads return data and compares against `expected_output` from `LogicVerifierInputs.app_data.external_payload`.

### How External Calls Are Encoded

External calls live in `LogicVerifierInputs.app_data.external_payload` within the RM transaction:

```rust
pub struct SolanaExternalCall {
    pub program_id: [u8; 32],       // Forwarder program ID
    pub instruction_data: Vec<u8>,  // Passed as `input` to forward_call
    pub expected_output: Vec<u8>,   // Must match return data
    pub output_mode: OutputMode,
}
```

The fixture generator (`tools/fixture-gen`) encodes this structure with the forwarder's program ID, a timestamp, and the expected comparison result.

### Assumption Boundary (EVM Parity)

The Solana PA intentionally mirrors the EVM PA trust model:

1. Aggregation proof verification is bound to per-LVI journal bytes re-derived from `LogicVerifierInputs.app_data` via `LogicInstance::to_journal()` — a hand-rolled risc0-serde encoder in `arm_core` matching EVM's `RiscZeroUtils.toJournal`.
2. External call execution and output checks read the same `LogicVerifierInputs.app_data.external_payload`.
3. Because (1) and (2) share that field, any mutation of `app_data` flows into the aggregation digest and invalidates the Groth16 proof — no duplicated wire field can diverge.

### How the Test Passes Accounts

The test provides the forwarder and its dependencies via `remaining_accounts`:

```typescript
const allRemainingAccounts = [
  ...nullifierPdas,  // Writable PDAs for nullifier marking
  { pubkey: blockTimeForwarderId, isWritable: false, isSigner: false },
  { pubkey: SYSVAR_CLOCK_PUBKEY, isWritable: false, isSigner: false },
];
```

The PA iterates through external calls, consuming accounts from this list for each CPI.

### Create Your Own Forwarder

1. **Scaffold the program**:
   ```bash
   cd solana-pa-prototype
   anchor new my-forwarder
   ```

2. **Implement `forward_call`** in `programs/my-forwarder/src/lib.rs`:
   ```rust
   use anchor_lang::prelude::*;
   use anchor_lang::solana_program::program::set_return_data;

   declare_id!("...");  // Generated after first build

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

3. **Build and sync program ID**:
   ```bash
   anchor build -p my-forwarder
   solana-keygen pubkey target/deploy/my_forwarder-keypair.json
   # Update declare_id!() with this value, then rebuild
   ```

4. **Update `fixture-gen`** to encode a `SolanaExternalCall` with your forwarder's program ID, input, and expected output.

5. **Update tests** to include your forwarder's program and required accounts in `remaining_accounts`.

---

## Building a Client

Clients submit RM transactions to the PA for settlement.

### Transaction Flow

1. **Upload RM transaction** (if >1232 bytes):
   ```typescript
   // Initialize TxData account
   await program.methods.txdataInit(uploadId, capacity, expiresSlot)
     .accounts({ txData, authority, systemProgram })
     .rpc();

   // Write chunks (max ~700 bytes per chunk)
   for (let offset = 0; offset < payload.length; offset += 700) {
     await program.methods.txdataWrite(uploadId, offset, chunk)
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
       verifierRouterProgram,
       router: routerPda,
       verifierEntry: verifierEntryPda,
       verifierProgram: groth16VerifierId,
     })
     .remainingAccounts([
       // Nullifier marker PDAs (one per consumed resource)
       ...nullifierPdas.map(pubkey => ({ pubkey, isWritable: true, isSigner: false })),
       // External call accounts (forwarder program + its required accounts)
       { pubkey: forwarderProgramId, isWritable: false, isSigner: false },
       { pubkey: forwarderAccount1, isWritable: false, isSigner: false },
       // ... more forwarder accounts
       // New root marker PDA (last position, optional)
       { pubkey: newRootMarkerPda, isWritable: true, isSigner: false },
     ])
     .preInstructions([
       ComputeBudgetProgram.setComputeUnitLimit({ units: 1_400_000 }),
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

The PA expects `remaining_accounts` in this order:

1. **Nullifier PDAs** (N accounts, writable) - One per consumed nullifier in the transaction
2. **External call segments** - For each external call:
   - Forwarder program account (executable)
   - Accounts required by that forwarder
3. **Historical root markers** (optional, read-only) - For transactions anchored to historical roots
4. **New root marker PDA** (optional, last position, writable) - Created after settlement

---

## External Call Encoding

External calls are encoded in the RM transaction's `LogicVerifierInputs.app_data.external_payload`.

### SolanaExternalCall Structure

```rust
#[derive(Serialize, Deserialize)]
pub struct SolanaExternalCall {
    pub program_id: [u8; 32],       // Forwarder program ID
    pub instruction_data: Vec<u8>,  // Passed to forwarder's forward_call
    pub expected_output: Vec<u8>,   // Must match forwarder's return data
    pub output_mode: OutputMode,
}

#[derive(Serialize, Deserialize)]
pub enum OutputMode {
    ReturnData,  // Read via get_return_data() (≤1024 bytes)
}
```

### Encoding

External calls are serialized with bincode, then converted to a word array:

```rust
let bytes = bincode::serialize(&call)?;
let words = bytes_to_words(&bytes);  // Pads to 4-byte boundary, little-endian

ExpirableBlob {
    blob: words,
    deletion_criterion: 0,
}
```

---

## Fixtures

Fixtures contain pre-generated RM transactions with valid Groth16 proofs. Required for testing because proof generation is expensive (~2 hours per fixture).

### Fixture Format

```json
{
  "format": "solana-pa-fixture-v1",
  "aggregation_strategy": "batch",
  "aggregation_proof_type": "groth16",
  "selector": "0x73c457ba",
  "tx_b64": "<base64 encoded RM transaction>",
  "tx_tampered_b64": "<base64 encoded tampered transaction>",
  "consumed_nullifiers_b64": ["<base64>", ...]
}
```

### Generating Fixtures

Proofs are dispatched to the AnomaPay workers queue. Set `QUEUE_BASE_URL` and `QUEUE_AUTH_TOKEN` in your environment first.

```bash
cd solana-pa-prototype
cargo run --locked --manifest-path tools/fixture-gen/Cargo.toml --release -- tests/fixtures/batch_groth16.json
```

Generate the mismatch fixture used by the `ExternalCallOutputMismatch` test:
```bash
cargo run --locked --manifest-path tools/fixture-gen/Cargo.toml --release -- --output-mismatch tests/fixtures/batch_groth16_mismatch.json
```

`fixture-gen` builds `passthrough-logic-guest` in Docker during build/startup.
Docker is required because the guest is compiled with the RISC0 guest toolchain (`cargo +risc0` and the RISC-V C toolchain) provided by the `risczero/risc0-guest-builder` image.
Keep Docker running, and no extra cargo feature flags are required.

### Fixture Staleness

Fixtures embed the `block_time_forwarder` program ID. If that ID changes, fixtures must be regenerated. The test script detects this automatically.

---

## Deployment

### Local Testing

The local test validator automatically clones RISC0 verifier programs from devnet. This is configured in `Anchor.toml`:

```toml
[test.validator]
url = "https://api.devnet.solana.com"

[[test.validator.clone]]
address = "BetEAE4npinksQBxvqUN1KkCVjYFJywWao45MSWtp5yg"  # Verifier Router

[[test.validator.clone]]
address = "2Yfa83Lzbn71ie3J1KQRiNQz1qHnvVm8gkBCpXZQ7ajD"  # Groth16 Verifier

[[test.validator.clone]]
address = "9ZJmYSYaYq38GfwQMsEw5gkzfr94Vbzw6Nv53yQuCv2S"  # Router PDA (initialized state)

[[test.validator.clone]]
address = "4ktbrXwBXZMoND5qb3J6abS1m8KqwUtCjjDBebJ4vqey"  # Verifier Entry PDA (groth16 selector registered)
```

### Start Validator Manually

```bash
cd solana-pa-prototype
./scripts/dev.sh validator
```

The validator is configured to clone the RISC0 verifier programs from devnet on startup.

### Deploy PA Programs

```bash
cd solana-pa-prototype
solana config set --url http://127.0.0.1:8899
anchor deploy --provider.cluster http://127.0.0.1:8899
```

---

## Troubleshooting

### `nix develop` Fails

Ensure flakes are enabled:
```bash
nix --extra-experimental-features 'nix-command flakes' develop
```

### Unsupported Host: `aarch64-linux`

The pinned Agave `v3.0.13` release does not publish `aarch64-unknown-linux-gnu` binaries.  
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

Program ID in source doesn't match keypair. The `anchor-test.sh` script syncs IDs automatically. If you see this error, try:
```bash
./scripts/dev.sh clean
./scripts/dev.sh anchor-test
```

### Build Fails on "verifier_router" Dependency

Ensure you're on a branch that uses git dependencies (not local paths):
```bash
git checkout feature/remove-submodule-with-pkg-deps
./scripts/dev.sh clean
./scripts/dev.sh anchor-test
```

### Fixture Generation Fails

`fixture-gen` needs Docker specifically for guest compilation (`passthrough-logic-guest`) via the RISC0 guest-builder image.
Then verify Docker is available, and that `fixture-gen` can compile and start:
```bash
docker --version
cargo check --locked --manifest-path tools/fixture-gen/Cargo.toml
timeout 8 tools/fixture-gen/target/debug/fixture-gen tools/fixture-gen/target/tmp-fixture-check.json
```

### Slow First Build

The first build downloads and compiles many dependencies (~10-20 minutes). Subsequent builds reuse the local Nix/cargo caches and are much faster.
