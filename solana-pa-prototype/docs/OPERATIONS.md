# Operating a Protocol Adapter Deployment

This document is the operator's procedure set for a Protocol Adapter (PA) deployment: how to deploy and initialize it, run it, stop it in an emergency, and retire it permanently. The one fact that shapes everything here: **Solana programs are upgradeable, so "stopped forever" is not enforced by the chain — it is enforced by how you handle two keys.** The EVM Protocol Adapter gets finality for free because its contract is immutable — its `emergencyStop()` has no unpause, no key can resurrect a stopped instance, and its only operational document is a deploy/release checklist. On Solana the equivalent finality is an operator action (burning the upgrade authority), it must come last, and the stop/retire half of the lifecycle below is the part EVM immutability does automatically.

All commands run through `./scripts/dev.sh` from `solana-pa-prototype/`, which enters the Nix shell automatically. Every command takes `--cluster <localnet|devnet|mainnet>`; see `scripts/ops.sh` for all flags.

## The two keys

A deployment has two independent authorities:

| Authority | Lives in | Controls | Moved by |
|---|---|---|---|
| PA authority | `PAStateAccount.authority` | `emergency_stop`, `update_expiry_config`, authority transfer | `propose_authority` + `accept_authority` (two-step, on chain) |
| Upgrade authority | BPF loader's ProgramData account | replacing the program binary; signing `initialize`; final immutability | `solana program set-upgrade-authority` |

They start as the same key: `initialize` requires its payer to be the program's upgrade authority, and records that payer as the initial PA authority (`programs/solana-pa-prototype/src/lib.rs`, the `Initialize` accounts constraint and handler). After initialization no instruction ever compares them, so they can be split freely — the integration suite exercises operation with them split (`tests/solana-pa-prototype.ts`, the authority transfer tests).

Two consequences to keep in mind:

- The PA authority alone decides an emergency stop, regardless of who can upgrade the program.
- Whoever holds the upgrade authority can replace the program binary — which means they could deploy code that undoes a stop. A stop is only as permanent as upgrade-authority custody.

Current key custody per cluster lives in the deployment record (`docs/DEVNET_DEPLOYMENT.md` for devnet), which is updated after every operation.

## Deploy and initialize

Prerequisites:

1. A funded wallet for the target cluster (per-cluster defaults and the `--wallet` flag: `scripts/ops.sh` usage). `deploy` checks the balance against its own size-based estimate before building and refuses with the amount needed.
2. The two initialization parameters, exported as environment variables. `initialize` pins them for the lifetime of the deployment — there is no safe default and no way to change them later:

   ```sh
   export PA_VERIFIER_ROUTER=<verifier router program ID>
   export PA_PROOF_SELECTOR=<4-byte hex selector>
   ```

   Running `deploy pa` without them set prints the current devnet values, which are defined once in `scripts/validator-deploy.sh` and recorded in the cluster's deployment record.
3. Program keypairs present under `target/deploy/` (they are committed to git and restored automatically). Cluster deploys refuse to invent fresh program IDs; if you intend a new ID, generate keypairs with `./scripts/dev.sh anchor-build` and commit the ones you deploy.

Procedure:

```sh
./scripts/dev.sh deploy pa --cluster devnet        # or: deploy all
./scripts/dev.sh status --cluster devnet           # verify: deployed + PAState initialized
./scripts/dev.sh idl-publish --cluster devnet      # put the production IDL on chain
```

`deploy` builds the production binary by default and verifies that `close_markers_batch` — a development-only instruction that deletes nullifier markers, i.e. replay protection — is absent from it. Passing `--dev-teardown` opts into the development build, which is the only build whose markers can later be reclaimed by `close-pdas`.

`idl-publish` stores the production IDL in the program's Anchor IDL account on chain, so explorers and generic Anchor clients decode the deployment's instructions and events without out-of-band files. It rebuilds the production IDL (which self-checks that no dev-only instruction leaks into it), then verifies the cluster serves exactly the published file. Rerun it after every `upgrade` that changes the interface.

The program's display metadata — name, icon, description, project links, and the security contact (`security@anoma.foundation`, same as the EVM PA's `@custom:security-contact`) — lives in `docs/program-metadata.json` and is published to the program-metadata PDA that Solana Explorer reads:

```sh
npx @solana-program/program-metadata@latest write security <PROGRAM_ID> \
  docs/program-metadata.json --rpc <rpc-url> --keypair scripts/devnet-wallet.json
```

Signer must be the program's upgrade authority. Republish after changing the JSON. The logo URL is the public Anoma GitHub org avatar: this repository is private, so assets in it (`docs/assets/anoma-logo.jpeg`) are not fetchable by explorers — a publicly served URL is required.

After a first deployment, update `docs/DEVNET_DEPLOYMENT.md` (or the equivalent record for the cluster) with the program IDs, wallet, router, selector, and date.

To ship new code to an existing deployment: `./scripts/dev.sh upgrade --cluster <c>`. Upgrading replaces the binary in place; it does not touch PAState, markers, or the initialization parameters.

### Upgrades that change the state layout

`upgrade` replaces code only. The state account (`PAStateAccount`, PDA seed `pa_state`) keeps whatever bytes it had, so a binary whose account layout differs from the deployed one cannot read it. The adapter makes that failure explicit instead of accidental:

- Byte 8 of the account data (the first byte after Anchor's discriminator) is the **schema version**, written by `initialize` from `PAStateAccount::SCHEMA_VERSION`. It is a layout number and changes only when the layout does.
- Every instruction that reads the state account refuses it when byte 8 is not the binary's own version. `txdata_write`, `txdata_close`, and `txdata_close_expired` never load `PAStateAccount`, so they keep working against a foreign version regardless of migration status, letting uploaders reclaim rent mid-migration. An upgrade to a layout-changing binary therefore stops the rest of the adapter cold until the account is migrated; nothing misreads old bytes.
- The refusal surfaces as one of two errors depending on how the layout changed. When the old account still deserializes under the new binary's layout — a version-byte mismatch only — the error is `UnsupportedStateSchema`. When the layout change itself makes the account undeserializable (for example a newer binary reading a shorter, older account), Anchor's `AccountDidNotDeserialize` surfaces first, because account deserialization runs before constraints. Either way the instruction is refused before it runs.

A release that changes the layout must ship the migration with it, and the procedure is: upgrade the binary, then run its `migrate_state` instruction once, signed by the PA authority and the upgrade authority, before any other instruction. That instruction reads the account as raw bytes, requires the previous version at byte 8, reallocates the account to the new size, rewrites it in the new layout with whatever new parameters the layout needs, and sets the new version. Because the account is not readable through the typed layout at that point, `migrate_state` declares it as an unchecked account and re-derives or receives the PDA bump instead of reading `bump` from the account. The operator running it pays the rent difference. Layout changes append fields after the existing ones so that the version byte and every earlier field keep their offsets. The V2 binary ships no `migrate_state` because it is deployed fresh; the first layout change after it must add one.

This covers the state account only. A change to the commitment tree itself (hash, arity, leaf encoding) or to the marker PDA seeds invalidates the existing tree and marker addresses, and no in-place migration recovers that: it is a fresh deployment plus a bulk copy of roots and nullifiers, the same limit the EVM adapter has.

The development build's `dev_set_schema_version` instruction exists only to test the refusal; the release build asserts it is absent, alongside `close_markers_batch`.

### Verified (reproducible) builds

The deployed PA should be the deterministic `solana-verify` Docker build, so the on-chain bytes are checkable against this repo:

```sh
./scripts/dev.sh verify-build --cluster devnet      # build + compare against deployed
./scripts/dev.sh upgrade pa --cluster devnet --prebuilt   # ship that exact artifact
```

`verify-build` builds with the pinned image (`[workspace.metadata.cli]` in `Cargo.toml` selects it — keep it in lockstep with `flake.nix`) and fails loudly if the deployed program doesn't match. **A normal build overwrites the artifact with non-matching bytes** — after any `anchor-test` or `build-*`, rerun `verify-build` before an upgrade you intend to keep verified. Validate the artifact behaviorally before shipping: `dev.sh validator` (backgrounded), `dev.sh deploy --cluster localnet --prebuilt`, `dev.sh anchor-test --cluster localnet --prebuilt`.

The deployment is then reproduced and checked with:

```sh
solana-verify verify-from-repo -u <rpc> --program-id <PROGRAM_ID> \
  https://github.com/anoma/solana-protocol-adapter --mount-path solana-pa-prototype \
  --library-name protocol_adapter
```

**Reach depends on repo visibility.** This repository is private, so today only people with read access (their git credentials satisfy the clone) can run the check; the on-chain verification PDA points at a repo outsiders cannot fetch. The explorer "Verified" badge requires more on both axes: the OtterSec remote API serves mainnet only, and its worker must be able to clone the repo — i.e. the source (at least at the recorded commit) must be public. A mainnet deployment that should carry the badge therefore requires opening the source; that is a product decision, not an operational step.

## The settlement lookup table

Every settlement carries about fifteen accounts that never change for a deployment: PAState, the verifier router and its entry, the verifier program, the event authority, the two forwarders and the SPL forwarder's config, the sysvars, the SPL token program, and each supported mint's escrow PDA and escrow ATA. Submitters send settlements as v0 transactions against an address lookup table holding those keys, which costs one byte per key instead of 32 and keeps the first-wrap settlement (ed25519 authorization, inline bitmap init, settle) well inside the 1,232-byte packet.

```sh
./scripts/dev.sh lookup-table --cluster devnet                      # create
PA_LOOKUP_TABLE=<address> STF_TOKEN_MINTS=<mint>,<mint> \
  ./scripts/dev.sh lookup-table --cluster devnet                    # extend
```

The command derives the key set from the deployed programs and the PAState's pinned router and selector, so it runs after `deploy pa`. The signing wallet is the table's authority and stays so (the table is not frozen) because supporting a new mint means extending it. A table entry need not exist on chain: the forwarder's keys go in before the forwarder is deployed, and a mint's escrow keys before `forwarder init` creates the escrow. Extending is idempotent; a rerun adds only what is missing.

Record the address in the cluster's deployment record and ship it as `SETTLE_LOOKUP_TABLE` in anoma-pa-solana-client. A program-id rotation is a new deployment and gets a new table.

## The kind table

The PA stores the sha256 commitment of the kind table every settled aggregation instance must carry, and rejects a transaction proven against any other table. `initialize` installs the first commitment; the authority replaces it in place, as the EVM adapter's owner does with `setKindTableCommitment`:

```sh
PA_KIND_TABLE_COMMITMENT=<hex, 32 bytes> ./scripts/dev.sh set-kind-table --cluster <c>   # authority wallet
```

The instruction rejects a zero commitment and emits `KindTableCommitmentUpdatedEvent` with the previous and the new value. From that slot on, transactions proven against the previous table are rejected (`KindTableCommitmentMismatch`), so provers must load the new table before it is installed. Today every deployment stores the empty table's commitment (`e3b0c442…`, fixture-gen's committed `kind_table.json`); the generated Solana tables and their commitments come from anoma/risc0-kind-tables once its Solana branch exists (anoma/dos-pm#61).

## Emergency stop

The stop exists for one scenario: the deployment can no longer be trusted — typically a suspected vulnerability — and settlement must halt now.

```sh
./scripts/dev.sh estop --cluster devnet        # prints what will happen, then refuses
./scripts/dev.sh estop --cluster devnet --yes  # executes
```

The command must be signed by the PA authority. It flips the lifecycle flag from Running to Stopped and there is no instruction that flips it back.

What stops: `settle` and `settle_from_txdata` reject every transaction with `PAError::Stopped`. Those are the only two instructions gated on the lifecycle flag.

What keeps working: everything else. All accounts (PAState, the commitment tree, nullifier and root markers) remain on chain and readable forever. Transaction-data upload accounts can still be closed and their rent reclaimed by their owners. Authority transfer and expiry configuration still function.

What a stop does **not** do: it does not prevent the upgrade-authority holder from deploying a modified binary. If the stop is meant to be permanent, finish the job with the Sunsetting steps below.

### The second kill switch: the verifier

Settlement also depends on RISC0's verifier router (the program pinned at `initialize`). The router keeps its own per-verifier emergency stop, checked inside the router during the verification call — the PA does not check it and cannot: a failed cross-program invocation aborts the whole transaction, so the PA never sees the error to translate it. This mirrors the EVM PA, whose `isEmergencyStopped()` reports true if either its own pause or the RISC0 verifier's pause is set.

Diagnosis is by transaction logs: a router-side stop shows the router program's own `SelectorDeactivated` error in the failed transaction's logs, clearly distinct from a proof failure. Whether anyone outside this project can trip that switch depends on who owns the router deployment — the current owner is in the cluster's deployment record.

## Recovery

Recovery from a stopped PA is migration to a new deployment. There is no in-protocol recovery mechanism; the EVM PA makes the same choice.

Concretely, migration means: deploy a fresh PA under a new program ID (new keypair), initialize it, and have applications re-establish their state against the new deployment's empty commitment tree. The stopped deployment's tree, markers, and history remain on chain and readable forever, so nothing about the old state is lost as evidence — but resources committed to the old tree cannot be settled anywhere, and value they represent must be recovered at the application layer (each application proves what it owned in the old tree and re-issues it in the new one, under whatever policy its owners decide).

The PAState account of a stopped deployment can never be re-initialized. This is deliberate: the account address derives from a fixed seed, so re-initializing would resurrect old nullifier-marker addresses and let previously spent notes spend again.

## The SPL token forwarder

The SPL token forwarder (`programs/spl-token-forwarder`) holds AnomaPay's wrapped SPL tokens in escrow and executes the wrap and unwrap calls the adapter forwards to it. It has two authorities: the program's **upgrade authority**, which upgrades the program and rotates the logic ref, and the **emergency committee**, recorded in its config PDA at initialization, which names the emergency caller and closes accounts. All commands below go through `./scripts/dev.sh forwarder <command> --cluster <c>`; their parameters are `STF_*` environment variables (`scripts/forwarder.ts` lists them).

### Deploy and initialize

```sh
export STF_LOGIC_REF=<32-byte hex verifying key of the AnomaPay resource logic>
export STF_EMERGENCY_COMMITTEE=<base58 pubkey>
export STF_TOKEN_MINT=<base58 mint>          # optional: also creates the mint's escrow ATA
./scripts/dev.sh deploy stf --cluster devnet  # or: forwarder init, for an already deployed program
```

The config pins the adapter program id, the logic ref, and the committee. A wrap is only executed when the adapter forwards it for a resource carrying that logic ref. Escrow accounts are associated token accounts owned by a per-mint PDA; `forwarder init` with `STF_TOKEN_MINT` creates one, and the same command adds further mints later. Add each new mint's escrow accounts to the settlement lookup table as well (`lookup-table` with `STF_TOKEN_MINTS`).

### Nonce bitmaps

A wrap's replay protection is a per-user, per-256-nonce-word bitmap account. The adapter forwards no signer to the forwarder, so the forwarder cannot create that account during a wrap; the submitter creates it with the permissionless `init_nonce_bitmap` instruction (any payer) when the word's bitmap does not exist, and the wrap fails with `NonceBitmapMissing` when it is absent. The init fits in the settlement transaction itself, after the ed25519 instruction the wrap input points at; the integration suite settles the first wrap that way.

### Rotating the logic ref

The logic ref changes whenever the resource circuit is rebuilt. The upgrade authority rotates it in place, as the EVM forwarder's owner does through its upgrade path:

```sh
STF_LOGIC_REF=<new 32-byte hex verifying key> ./scripts/dev.sh forwarder set-logic-ref --cluster <c>   # upgrade-authority wallet
```

Escrow, nonce bitmaps and the committee are untouched; the config emits `LogicRefSet` with the previous and the new ref. Resources wrapped under the previous ref stay in escrow: the new logic cannot unwrap them, and moving them is the migration path (anoma/dos-pm#77), not a rotation concern. The emergency path below is for a stopped adapter only.

### Emergency committee

Once the adapter is stopped (`estop`), the committee names an emergency caller, once, and that caller withdraws from escrow directly without going through the adapter:

```sh
STF_EMERGENCY_CALLER=<pubkey> ./scripts/dev.sh forwarder set-emergency-caller --cluster <c>   # committee wallet
STF_TOKEN_MINT=<mint> STF_RECIPIENT=<owner> STF_AMOUNT=<raw units> \
  ./scripts/dev.sh forwarder emergency-withdraw --cluster <c>                                # caller wallet
```

`set-emergency-caller` refuses while the adapter is running and cannot be repeated. The committee can also drain and close an escrow outright with `drain-escrow`, at any time.

### Retiring the forwarder

`STF_TOKEN_MINT=<mint> ./scripts/dev.sh forwarder teardown --cluster <c>` (committee wallet) closes every nonce bitmap, drains and closes that mint's escrow to the committee, and closes the config, reclaiming their rent. Run it once per mint that has an escrow, then close the program with `teardown stf`.

## Sunsetting

Permanent retirement, in order. The order matters because `initialize` requires a live upgrade authority: once the authority is gone, that program ID can never host a PA again — which is the point, but only as the final step.

1. **Stop settlement.** `./scripts/dev.sh estop --cluster <c> --yes`.
2. **Reclaim marker rent — development builds only.** `./scripts/dev.sh close-pdas --cluster <c>` closes nullifier and root markers via `close_markers_batch`, which requires the Stopped state and exists only in `--dev-teardown` builds. Production builds abandon marker rent by design; there is deliberately no production path that deletes replay-protection markers. The command validates the instruction against the locally built IDL, so run a development build first (`./scripts/dev.sh run "./scripts/ops.sh build-dev"`) if the last build was a production one — observed during the first devnet retirement.
3. **End the program.** Two mutually exclusive options:
   - `solana program close <PROGRAM_ID> --bypass-warning` — reclaims the program account's rent and burns the program ID permanently (`./scripts/dev.sh teardown --cluster <c>` does steps 2 and 3 together), or
   - `solana program set-upgrade-authority <PROGRAM_ID> --final` — keeps the stopped program on chain forever but makes it immutable: no future upgrade can undo the stop, and the state remains readable at its original addresses.

For a beta deployment on devnet, closing the program (reclaiming rent) is the normal end. Immutability-by-burned-authority is the shape a mainnet retirement would take when the historical state should stay served at its known addresses.

