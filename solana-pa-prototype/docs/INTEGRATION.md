# Protocol Adapter Integration Contract

This document is the contract for software that talks to the Protocol
Adapter (PA) on chain: services that build and submit transactions, indexers
that reconstruct state from its events, explorers, and client bindings. It
covers the four things the README's client walkthrough does not: the exact
wire format of a transaction, the transaction-data upload account's
semantics, the full event set with emission rules, and the root-marker model.

Call-level mechanics — instruction call sequences, account lists, PDA
derivations, the `remaining_accounts` layout, and external-call encoding —
are in the repo README under "Building a Client" and are not repeated here.
Canonical TypeScript derivations live in `tests/utils/constants.ts` and
`tests/utils/pda.ts`. Where this document names source files, they are under
`programs/solana-pa-prototype/src/`.

## Two submission paths

- **`settle(transaction_data: Vec<u8>)`** — one instruction carrying the
  whole serialized transaction. Only usable when the transaction fits in a
  single Solana transaction.
- **`txdata_init` → `txdata_write` (repeated) → `settle_from_txdata`** —
  upload the serialized transaction in chunks to a buffer account, then
  settle from it. This is the normal path; real transactions with proofs do
  not fit in one Solana transaction.

Both paths verify the same things and emit the same events. Settlement is
rejected while the deployment is emergency-stopped.

## The transaction wire format

There are two serialization layers, and confusing them is the most common
integration mistake:

1. **The instruction layer is Anchor's:** an 8-byte instruction
   discriminator followed by Borsh-serialized arguments. Any Anchor client
   handles this automatically.
2. **The `transaction_data` bytes inside that argument are bincode, not
   Borsh.** The PA deserializes them with `bincode::deserialize` into the
   `Transaction` type from the `anoma-rm-core` crate
   (`github.com/anoma/arm-risc0`, branch `solana` — the exact commit is
   pinned in this repo's `Cargo.lock`). Producers must serialize with
   bincode from that same crate version; the encoding must match
   byte-for-byte.

Borsh appears elsewhere in the PA (account state, event bodies, instruction
arguments) but never for the transaction payload itself.

Working examples: every fixture in `tests/fixtures/*.json` carries a
complete valid transaction as base64 in its `tx_b64` field.

A transaction must carry an aggregation proof (`aggregation_proof` set), and
its seal's 4-byte selector must equal the one this deployment pinned at
initialization — see "Deployment parameters" below.

## Transaction-data upload accounts (TxData)

The chunked path's buffer account, created per upload. Facts an integrator
must know (source: the `txdata_*` handlers in `lib.rs` and the account
constraints below them):

- **Identity:** PDA of `["tx_data", authority, upload_id as u64 LE]`. The
  `upload_id` is chosen by the uploader; one authority can run parallel
  uploads under distinct IDs.
- **Roles:** the account records an `authority` and a `refund` address, both
  set to the creating signer. Only the authority can write chunks, extend
  the deadline, settle from the account, or close it early. Rent always
  returns to `refund`.
- **Expiry:** `txdata_init` takes an `expires_slot`, which must land between
  `min_expiry_slots` and `max_expiry_slots` from the current slot. These
  bounds live on the PA state account and are operator-tunable within
  [10 slots, 7 days]; read them from chain rather than assuming the
  defaults. Writes and settlement are rejected after expiry.
  `txdata_extend` can push the deadline out (strictly increasing, same
  bounds).
- **Garbage collection is permissionless:** after expiry, anyone may call
  `txdata_close_expired`; the rent still goes to the stored `refund`
  address. Uploads that are abandoned do not leak rent forever.
- **Capacity is fixed at init.** The account is allocated at that size up
  front. Settlement deserializes only the bytes actually written
  (`payload[..written_len]`), so capacity must be at least the serialized
  transaction length; the tests size it exactly.

## Events

All events are Anchor events: base64 payloads in the program log, prefixed
with an 8-byte discriminator derived from the event's name, body
Borsh-encoded. A settlement emits, in this order:

1. Per action, per resource, per payload entry: one **payload event**
   (see filtering rule below).
2. Per action: `ActionExecutedEvent { action_tree_root: [u8;32],
   action_tag_count: u32 }`.
3. Per external call, in call order: `ForwarderCallExecutedEvent
   { forwarder: Pubkey, input: Vec<u8>, output: Vec<u8> }`.
4. Once: `TransactionExecutedEvent { tags: Vec<[u8;32]>,
   logic_refs: Vec<[u8;32]> }`.

**Tag semantics** (`extract_tags_and_logic_refs` in `encoding.rs`): for each
compliance unit of each action, two tags are appended in order — the
consumed resource's nullifier, then the created resource's commitment.
`logic_refs` is index-parallel to `tags`. `TransactionExecutedEvent`
therefore lists every state change of the settlement: even-indexed entries
are nullifiers, odd-indexed entries are commitments, and the commitments
appear in exactly the order they were appended to the commitment tree.

**Payload events** carry application data blobs. There are four structs with
identical bodies `{ tag: [u8;32], index: u32, blob: Vec<u8> }` —
`ResourcePayloadEvent`, `DiscoveryPayloadEvent`, `ExternalPayloadEvent`,
`ApplicationPayloadEvent` — one per payload category. Four distinct structs
exist so each category gets its own Anchor discriminator and indexers can
filter at the log-parsing level without decoding bodies. Rules
(`emit_app_data_events` in `lib.rs`):

- A payload entry is emitted **only if its deletion criterion says "store
  forever"** (`deletion_criterion == 1`). Entries with any other criterion
  never appear in events; an indexer cannot reconstruct them and must not
  expect to.
- `tag` is the resource tag the payload belongs to; `index` is the entry's
  position within its own category's payload list for that resource (not a
  global index).
- `blob` is the payload's `u32` word array reinterpreted as bytes in memory
  order (`words_to_bytes`, a plain cast — little-endian on Solana), the
  inverse of the zero-padded `bytes_to_words` packing.

## Roots and markers

The PA does not store a list of historical roots in its state account.
Validity is existence of a marker account: a root is acceptable for
settlement if a PDA of `["root", pa_state, root_bytes]` owned by the PA
exists (`root.rs`). Two roots are valid without any marker: the current tree
root, and the empty-tree root (which equals the tree's padding leaf — this
lets transactions built against a freshly initialized PA settle without a
genesis marker).

For transaction builders:

- A transaction consuming resources proven against an older root must
  include that root's marker account (read-only) among the settlement's
  `remaining_accounts`; position does not matter. Omitting it fails the
  settlement with `NonExistingRoot`.
- Every settlement must supply the **new** root's marker address as the
  named `new_root_marker` account (writable) of `settle` /
  `settle_from_txdata` — it is not part of `remaining_accounts`, and it is
  required, not optional: the PA rejects with `RootPdaMismatch` unless the
  address matches the post-settlement root, which the builder computes by
  applying the transaction's commitments to the current tree. The PA
  creates the marker there, permanently: every settlement's resulting root
  stays valid, which is what allows building transactions against a root
  while other settlements advance the tree.

Markers are never deleted in production deployments. (Development builds
carry an extra teardown instruction; it cannot exist in a production binary
and integrators should ignore it.)

## Deployment parameters

`initialize` pins two values for the deployment's lifetime, readable from
the PA state account:

- `verifier_router: Pubkey` — the RISC0 verifier router the PA will call for
  proof verification. Settlement requires passing this exact program (plus
  its router PDA, verifier entry, and verifier program) in the settle
  accounts.
- `proof_selector: [u8;4]` — the verifier selector every transaction's
  aggregation seal must carry.

Program IDs, the router addresses for the current devnet deployment, and
key custody are in `docs/DEVNET_DEPLOYMENT.md`. Operator procedures
(deploy, emergency stop, retirement) are in `docs/OPERATIONS.md`; the fact
integrators care about: a stopped deployment rejects all settlement forever,
and recovery is a new deployment with a fresh, empty tree.
