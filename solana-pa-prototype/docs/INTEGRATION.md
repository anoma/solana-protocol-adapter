# Protocol Adapter Integration Contract

This document is the contract for software that talks to the Protocol Adapter (PA) on chain: services that build and submit transactions, indexers that reconstruct state from its events, explorers, and client bindings. It covers the four things the README's client walkthrough does not: the exact wire format of a transaction, the transaction-data upload account's semantics, the full event set with emission rules, and the root-marker model.

Call-level mechanics — instruction call sequences, account lists, PDA derivations, and the `remaining_accounts` layout — are in the repo README under "Building a Client" and are not repeated here. Canonical TypeScript derivations live in `client/constants.ts` and `client/pda.ts`, and the instruction builders the operator scripts and tests share in `client/instructions.ts`. Where this document names source files, they are under `programs/solana-pa-prototype/src/`.

## Two submission paths

- **`settle(transaction_data: Vec<u8>)`** — one instruction carrying the whole serialized transaction. Only usable when the transaction fits in a single Solana transaction.
- **`txdata_init` → `txdata_write` (repeated) → `settle_from_txdata`** — upload the serialized transaction in chunks to a buffer account, then settle from it. This is the normal path; real transactions with proofs do not fit in one Solana transaction.

Both paths verify the same things and emit the same events. Settlement is rejected while the deployment is paused.

## The transaction wire format

There are two serialization layers; do not confuse them:

1. **The instruction layer is Anchor's:** an 8-byte instruction discriminator followed by Borsh-serialized arguments. Any Anchor client handles this automatically.
2. **The `transaction_data` bytes inside that argument are bincode, not Borsh.** The PA deserializes them with `bincode::deserialize` into the `Transaction` type from the `anoma-rm-core` crate (crates.io; source `github.com/anoma/arm-risc0`), at the exact version pinned in `programs/solana-pa-prototype/Cargo.toml` and `Cargo.lock`. Producers must serialize with bincode from that same crate version; the encoding must match byte-for-byte.

Borsh appears elsewhere in the PA (account state, event bodies, instruction arguments) but never for the transaction payload itself.

Working examples: the `tests/fixtures/batch_groth16*.json` fixtures other than `batch_groth16_mismatch.json` carry complete settlement-ready transactions as base64 in their `tx_b64` field. The other fixtures (including `batch_groth16_mismatch.json`) are deliberately failing variants (missing aggregation, garbage proof, wrong root, forwarder output mismatch and failures) — useful as negative examples, not as templates.

A transaction must carry an aggregation (`aggregation` set, `actions` absent), and the 4-byte selector in `aggregation.proof`'s seal must equal the `proof_selector` this deployment pinned at initialization — see "Deployment parameters" below.

### External call encoding

External calls ride inside the proof-backed aggregation instance, in each consumed or created resource's `app_data.external_payload` (`aggregation.instance.actions[i].consumed_publics[j]` / `created_publics[j]`); a resource's calls run when the settlement reaches that resource. Each call is a `SolanaExternalCall` (defined in `types.rs`):

```rust
pub struct SolanaExternalCall {
    pub program_id: [u8; 32],       // Forwarder program ID
    pub instruction_data: Vec<u8>,  // Passed to the forwarder's forward_call
    pub expected_output: Vec<u8>,   // Must match the forwarder's return data; must be
                                    // non-empty (EmptyExpectedOutput otherwise — Solana
                                    // cannot represent an explicit empty return)
    pub output_mode: OutputMode,    // ReturnData: read via get_return_data() (≤1024 bytes)
    pub num_accounts: u8,           // Accounts in this call's remaining_accounts segment,
                                    // including the forwarder program account
}
```

The call is bincode-serialized, packed into a word array with `bytes_to_words` (zero-pads to a 4-byte boundary), and stored as an `ExpirableBlob` in the external payload. **Every** `external_payload` entry is decoded and executed as a call — the decoder does not filter, so nothing else may be stored there. The canonical encoder (`external_calls::encode_external_call`) sets `deletion_criterion` to `0` (ephemeral), which also keeps calls from being re-emitted as payload events (see Events).

## Transaction-data upload accounts (TxData)

The chunked path's buffer account, created per upload. Facts an integrator must know (source: the `txdata_*` handlers in `lib.rs` and the account constraints below them):

- **Identity:** PDA of `["tx_data", authority, upload_id as u64 LE]`. The `upload_id` is chosen by the uploader; one authority can run parallel uploads under distinct IDs.
- **Roles:** the account records an `authority` and a `refund` address, both set to the creating signer. Only the authority can write chunks, extend the deadline, settle from the account, or close it early. Rent always returns to `refund`.
- **Expiry:** `txdata_init` takes an `expires_slot`, which must land between `min_expiry_slots` and `max_expiry_slots` from the current slot. These bounds live on the PA state account and are operator-tunable within the program's hard envelope (`MIN_ALLOWED_EXPIRY` to `SEVEN_DAYS_SLOTS`, `state.rs`); read them from chain rather than assuming the defaults. Writes and settlement are rejected after expiry. `txdata_extend` can push the deadline out (strictly increasing, same bounds).
- **Garbage collection is permissionless:** after expiry, anyone may call `txdata_close_expired`; the rent still goes to the stored `refund` address. Uploads that are abandoned do not leak rent forever.
- **Capacity is fixed at init.** The account is allocated at that size up front. Settlement deserializes only the bytes actually written (`payload[..written_len]`), so capacity must be at least the serialized transaction length; the tests size it exactly.

## Events

All events are Anchor CPI events. The program emits each event by invoking itself with the event as instruction data, signed by its event authority PDA (`["__event_authority"]`). Readers take the settlement transaction's inner instructions whose program is the adapter and whose data begins with the fixed 8-byte tag `e4 45 a5 2e 51 cb 9a 1d` — `anchor_lang::event::EVENT_IX_TAG_LE`, the little-endian encoding of the u64 `0x1d9acb512ea545e4`; the remaining bytes are the event's 8-byte discriminator (`sha256("event:<Name>")[..8]`) followed by the Borsh-encoded body. Events are not written to the program log, so the runtime's 10 KB per-transaction log limit cannot drop them; a settlement's events are complete exactly when the transaction succeeded. Both settle instructions take two additional accounts for this, appended after `verifier_program` and before the remaining accounts: `event_authority` (the PDA above, read-only) and `program` (the adapter's own address, read-only); Anchor's TypeScript client fills both in automatically, raw instruction builders append them explicitly. A settlement emits, in this order:

1. Per action, in action order, pa-evm's order: per resource (consumed resources first, then created), after its nullifier is recorded or its commitment appended, one `ForwarderCallExecutedEvent { forwarder: Pubkey, input: Vec<u8>, output: Vec<u8> }` per external call it carries, in its app data's order, then its **payload events** (one per emitted payload entry — see the filtering rule below); then the action's `ActionExecutedEvent { action_tree_root: [u8;32], nullifiers: Vec<[u8;32]>, consumed_logic_refs: Vec<[u8;32]>, commitments: Vec<[u8;32]>, created_logic_refs: Vec<[u8;32]> }`, pa-evm's `ActionExecuted`. Events of action N+1 therefore come after action N's `ActionExecutedEvent`.
2. Once, when the settlement appended commitments: `CommitmentTreeRootAddedEvent { root: [u8;32] }`, the root it produced.
3. Once: `TransactionExecutedEvent { transaction_id: [u8;32] }`, pa-evm's `TransactionExecuted`: the Keccak-256 hash of the concatenated action tree roots, the message the delta proof signs.

A forwarder emits its own events the same way, as inner instructions whose program is the forwarder; the SPL token forwarder's are documented in anoma/anomapay-spl-token-forwarder's `docs/INTEGRATION.md`.

A settlement's instruction trace includes its outer instructions, the verifier and forwarder calls, and one instruction per event above and per forwarder event; when that trace exceeds the runtime's limit of 64 instructions (`max_instruction_trace_length`), the settlement fails rather than emitting a truncated event set.

**Tag semantics** (`settle::action_resources` and `execute_settlement` in `lib.rs`): an action's `nullifiers` and `commitments` are its consumed and created resources' tags in instance order, and `consumed_logic_refs` / `created_logic_refs` are index-parallel to them. The commitments are appended to the commitment tree in exactly the order the `ActionExecutedEvent`s list them, action after action.

**Payload events** carry application data blobs. There are four structs with identical bodies `{ tag: [u8;32], index: u32, blob: Vec<u8> }` — `ResourcePayloadEvent`, `DiscoveryPayloadEvent`, `ExternalPayloadEvent`, `ApplicationPayloadEvent` — one per payload category. Four distinct structs exist so each category gets its own Anchor discriminator and indexers can filter on the discriminator without decoding bodies. Rules (`emit_app_data_events` in `lib.rs`):

- A payload entry is emitted **only if its deletion criterion says "store forever"** (`deletion_criterion == DELETION_CRITERION_NEVER`, defined in `state.rs`). Entries with any other criterion never appear in events; an indexer cannot reconstruct them and must not expect to.
- `tag` is the resource tag the payload belongs to; `index` is the entry's position within its own category's payload list for that resource (not a global index).
- `blob` is the payload's `u32` word array reinterpreted as bytes in memory order (`words_to_bytes`, a plain cast — little-endian on Solana), the inverse of the zero-padded `bytes_to_words` packing.

**Outside settlement**, these instructions emit pa-evm's events the same way, read from their own transaction's inner instructions; "owner only" means the owner stored in the adapter's state is the only signer accepted:

- `initialize`: `OwnershipTransferredEvent { previous_owner: Pubkey, new_owner: Pubkey }` from the zero key to the initial owner, OpenZeppelin Ownable's `OwnershipTransferred`; then `CommitmentTreeRootAddedEvent { root }` with the empty tree's root, then `KindTableCommitmentUpdatedEvent { kind_table_commitment: [u8;32] }` with the empty kind table's commitment.
- `transfer_ownership` / `renounce_ownership` (owner only): `OwnershipTransferredEvent` from the owner to the new owner, or to the zero key when renounced.
- `upgrade` (owner only): `UpgradedEvent { executable_hash: [u8;32] }`, ERC1967's `Upgraded`. A Solana program keeps its address across upgrades, so the new code is named by its executable hash: sha256 of the code without trailing zero bytes, which `solana-verify get-program-hash` reports. The new code runs from the next slot.
- `set_kind_table_commitment` (owner only): `KindTableCommitmentUpdatedEvent` with the new commitment, pa-evm's `KindTableCommitmentUpdated`. Transactions proven against the previous table are refused from then on.
- `deny_logic_ref` (owner only): `LogicRefDeniedEvent { logic_ref: [u8;32] }`, pa-evm's `LogicRefDenied`. No settlement consumes or creates a resource carrying that logic ref again, and a denial cannot be undone.
- `pause` / `unpause` (owner only): `PausedEvent { account: Pubkey }` / `UnpausedEvent { account: Pubkey }`, OpenZeppelin Pausable's `Paused` / `Unpaused`, where `account` is the signer. While paused, both settle instructions refuse every transaction.

## Roots and markers

A root is valid for settlement exactly when a marker account exists: a PDA of `["root", pa_state, root_bytes]` owned by the PA (`root.rs`). The PA stores no list of historical roots in its state account. Two roots are valid without any marker: the current tree root, and the empty-tree root (the tree pads unfilled positions with a fixed leaf value — `PADDING_LEAF` in `merkle.rs` — and an empty tree's root is built entirely from it, which lets transactions built against a freshly initialized PA settle without a genesis marker).

For transaction builders:

- A transaction consuming resources proven against an older root must include that root's marker account (read-only) in the settlement's `remaining_accounts`, after the nullifier-marker slots and forwarder segments — those leading positions are consumed positionally (see the README's layout), while the validity check itself scans the whole list. Omitting the marker fails the settlement with `NonExistingRoot`.
- A settlement that creates commitments must pass the **new** root's marker address as the optional `new_root_marker` account (writable) of `settle` / `settle_from_txdata`; it is not part of `remaining_accounts`. Compute the new root by applying the transaction's commitments to the current tree. If the address does not match the post-settlement root, settlement rejects with `RootPdaMismatch`. A settlement that creates nothing produces no root and must omit `new_root_marker`; passing one fails with `RootPdaMismatch`. The PA creates the marker at that address, permanently: every past root stays valid, so a builder can target a root while other settlements advance the tree.

Production binaries contain no instruction that deletes markers. (Development builds carry a teardown instruction, compiled out of production builds — a build-discipline boundary, not a chain guarantee; `dev.sh release-build` verifies its absence.)

## Deployment parameters

`initialize` pins two values for the deployment's lifetime, readable from the PA state account:

- `verifier_router: Pubkey` — the RISC0 verifier router the PA will call for proof verification. Settlement requires passing this exact program (plus its router PDA, verifier entry, and verifier program) in the settle accounts.
- `proof_selector: [u8;4]` — the 4-byte selector every transaction's aggregation proof must carry.

Program IDs, the router addresses for the current devnet deployment, and key custody are in `docs/DEVNET_DEPLOYMENT.md`. Operator procedures (deploy, pause, retirement) are in `docs/OPERATIONS.md`; the fact integrators care about: a paused deployment rejects settlement (`EnforcedPause`) until its owner unpauses it, and `PAStateAccount.paused` reports it.

The state account (`PAStateAccount`, PDA seed `pa_state`) carries its layout number at byte 8 of the account data, immediately after the Anchor discriminator. A client that decodes the account directly rather than through the published IDL must check that byte against the layout it was written for before reading further fields; every instruction that reads the account refuses a version other than the program's own, except a release's migration instruction, which accepts only the previous version and rewrites it into the current layout; so a mismatch a client sees is a deployment mid-migration, not corrupt data.
