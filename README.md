# Solana Protocol Adapter

Port of the [EVM Protocol Adapter](https://github.com/anoma/evm-protocol-adapter) to Solana, using RISC0 proving backend.

## Repository Structure

```
solana-protocol-adapter/
├── solana-pa-prototype/     # Solana PA implementation
├── risc0-solana/            # On-chain Groth16 proof verifier (submodule)
└── arm-risc0/               # RM implementation for zkVM (submodule)
```

## Local Development Setup

### Prerequisites

- Docker and docker-compose
- User in `docker` group (or use `sg docker -c "..."`)

### 1. Clone with Submodules

```bash
git clone --recurse-submodules https://github.com/anoma/solana-protocol-adapter
cd solana-protocol-adapter
```

### 2. Build Docker Environment

```bash
cd solana-pa-prototype
export UID="$(id -u)"
export GID="$(id -g)"
export DOCKER_GID="$(stat -c %g /var/run/docker.sock)"
docker-compose build
```

### 3. Build and Sync groth_16_verifier

The risc0-solana verifier ships with a placeholder program ID. Sync it to your local keypair:

```bash
docker-compose run --rm dev bash -c '
  cd /workspace/risc0-solana/solana-verifier
  anchor build -p groth_16_verifier
'
```

Get the generated program ID:

```bash
docker-compose run --rm dev solana-keygen pubkey /workspace/risc0-solana/solana-verifier/target/deploy/groth_16_verifier-keypair.json
```

Update both files with this pubkey:
- `risc0-solana/solana-verifier/programs/groth_16_verifier/src/lib.rs` line 32: `declare_id!("YOUR_PUBKEY");`
- `risc0-solana/solana-verifier/Anchor.toml` line 8: `groth_16_verifier = "YOUR_PUBKEY"`

Rebuild:

```bash
docker-compose run --rm dev bash -c '
  cd /workspace/risc0-solana/solana-verifier
  anchor build -p groth_16_verifier
'
```

### 4. Run Tests

```bash
./scripts/docker-dev.sh anchor-test
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

3. **Output via return data**: Use `set_return_data(&output)`. The PA reads this immediately after CPI and verifies it matches `expected_output` from the proof.

4. **Output account mode**: For outputs >1024 bytes, write to an account and use `OutputMode::OutputAccount` in the external call encoding.

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
./scripts/docker-dev.sh anchor-test
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

The PA's CPI call passes `logic_ref` and `input` extracted from the RM transaction. After the call, it reads return data and compares against `expected_output` embedded in the proof.

### How External Calls Are Encoded

External calls live in `LogicVerifierInputs.app_data.external_payload` within the RM transaction:

```rust
pub struct SolanaExternalCall {
    pub program_id: [u8; 32],       // Forwarder program ID
    pub instruction_data: Vec<u8>,  // Passed as `input` to forward_call
    pub expected_output: Vec<u8>,   // Must match return data
    pub output_mode: OutputMode,    // ReturnData or OutputAccount
}
```

The fixture generator (`tools/fixture-gen`) encodes this structure with the forwarder's program ID, a timestamp, and the expected comparison result.

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
   cd /workspace/solana-pa-prototype
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
    OutputAccount {
        index: u8,    // Index into remaining_accounts
        offset: u32,
        len: u32,
    },
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
  "consumed_nullifiers_b64": ["<base64>", ...],
  "created_commitments_b64": ["<base64>", ...]
}
```

### Generating Fixtures

```bash
# Aggregated (batch) fixture
docker-compose run --rm dev bash -lc '
  cd /workspace/solana-pa-prototype
  ./tools/fixture-gen/target/release/fixture-gen --threads 6 tests/fixtures/batch_groth16.json
'

# Non-aggregated fixture
docker-compose run --rm dev bash -lc '
  cd /workspace/solana-pa-prototype
  ./tools/fixture-gen/target/release/fixture-gen --non-aggregated --threads 6 tests/fixtures/individual_groth16.json
'
```

**Dev mode** (fake proofs, fast, won't verify on-chain):
```bash
RISC0_DEV_MODE=1 ./tools/fixture-gen/target/release/fixture-gen ...
```

### Fixture Staleness

Fixtures embed the `block_time_forwarder` program ID. If that ID changes, fixtures must be regenerated. The test script detects this automatically.

---

## Deployment

### Start Validator

```bash
docker-compose --profile validator up -d validator
```

### Deploy groth_16_verifier

```bash
docker-compose run --rm dev bash -c '
  solana config set --url http://solana-validator:8899
  solana program deploy \
    --program-id /workspace/risc0-solana/solana-verifier/target/deploy/groth_16_verifier-keypair.json \
    /workspace/risc0-solana/solana-verifier/target/deploy/groth_16_verifier.so
'
```

### Deploy PA Programs

```bash
docker-compose run --rm dev bash -c '
  cd /workspace/solana-pa-prototype
  solana config set --url http://solana-validator:8899
  anchor deploy
'
```

---

## Troubleshooting

### Docker Permission Denied

```bash
sg docker -c "docker-compose build"
```

### DeclaredProgramIdMismatch (Error 4100)

Program ID in source doesn't match keypair. For PA programs, `docker-anchor-test.sh` syncs automatically. For groth_16_verifier, see setup step 3.

### Fixture Generation Fails

Ensure Docker socket is accessible:
```bash
docker-compose run --rm dev docker ps
```
