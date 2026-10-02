# Operating a Protocol Adapter Deployment

This document is the operator's procedure set for a Protocol Adapter (PA) deployment: how to deploy and initialize it, upgrade it, run it, pause it, and retire it permanently. The one fact that shapes everything here: **the adapter is upgradeable by its owner, so "paused forever" is not enforced by the chain — it is enforced by custody of the ownership.** The same holds for pa-evm V2, a UUPS proxy its owner upgrades in place (`upgradeToAndCall`) and pauses and unpauses (`pause()`, `unpause()`); the Solana adapter has the same owner-only `upgrade`, `pause` and `unpause`. Renouncing the ownership, the last Sunsetting step, makes a pause permanent.

Commands run through `./scripts/dev.sh` from `solana-pa-prototype/`, which enters the Nix shell automatically; cluster operations take `--cluster <localnet|devnet|mainnet>` (see `scripts/ops.sh` for all flags). devnet and mainnet operations go through the operator's RPC provider: pass `--url <rpc>` or set `DEVNET_RPC_URL` / `MAINNET_RPC_URL`; there is no public-endpoint default. Commands shown as `solana`, `npx` or `solana-verify` run directly.

`dev.sh anchor-test --cluster <devnet|mainnet>` runs the local suite's tests against the live deployment, building on whatever history it holds: every spec file outside `tests/fresh/` and `tests/terminal/`, without the tests tagged `@localnet` (those need the owner or the forwarder's committee, change a deployment setting, or call a program deployed only on a local validator). The run first proves its own fixture set for the deployment, under a salt no earlier run used and against the kind table it stores, so name that table's JSON (as fixture-gen reads it) in `PA_KIND_TABLE`; proving runs locally unless `QUEUE_BASE_URL` sends it to the workers queue. Its wallet must own nothing under test: the run refuses a wallet that holds an owner's role: the adapter's stored owner, or a program's upgrade authority while that is not the program's own PDA. Use a separate funded test wallet (`--wallet`), and name the deployment's settlement lookup table in `PA_SETTLEMENT_TABLE` (without it the suite would create and leave a table of its own).

## The owner

The adapter has one owner, as pa-evm's (OpenZeppelin's OwnableUpgradeable): set by `initialize`, stored in the state account (`PAStateAccount.owner`), and the signer of every owner-only instruction (`upgrade`, `pause`, `unpause`, `update_expiry_config`, `set_kind_table_commitment`, `deny_logic_ref`, `transfer_ownership`, `renounce_ownership`; any other signer gets `OwnableUnauthorizedAccount`). `initialize` hands the program's upgrade authority from the deployer to the program's own PDA (seed `upgrade_authority`), so the loader accepts an upgrade only through the program's `upgrade`, for the owner, as a UUPS implementation authorizes its own upgrades. `initialize` announces the owner with `OwnershipTransferredEvent` from the zero key, as `OwnershipTransferred` is.

`transfer_ownership(new_owner)` moves the ownership at once (the zero key is refused, `OwnableInvalidOwner`); `renounce_ownership` gives it up for good: the owner becomes the zero key, and no owner-only instruction, `upgrade` included, can run again. Both emit `OwnershipTransferredEvent`. They are made by hand: no repository command or script changes an authority or closes protocol accounts on a live cluster, and the only builders of those instructions in the repository (`tests/utils/localOnly.ts`) refuse any endpoint but a local validator. The owner constructs and signs the instruction from the IDL.

The SPL token forwarder has an owner of its own, set by its `initialize` (`STF_OWNER`) and stored in its config, the same way: its upgrade authority is its own PDA, its `upgrade`, `reinitialize`, `transfer_ownership` and `renounce_ownership` are owner-only, and it emits `OwnershipTransferred` and `Upgraded`, as the EVM V2 forwarder's OwnableUpgradeable and UUPS do.

The owner can replace the program binary, which means it could deploy code that undoes a stop: a stop is only as permanent as the ownership's custody. Current key custody per cluster lives in the deployment record (`docs/DEVNET_DEPLOYMENT.md` for devnet), which is updated after every operation.

## Deploy and initialize

Prerequisites:

1. A funded wallet for the target cluster (per-cluster defaults and the `--wallet` flag: `scripts/ops.sh` usage). `deploy` checks the balance against its own size-based estimate before building and refuses with the amount needed.
2. The three initialization parameters, exported as environment variables. `initialize` makes `PA_OWNER` the owner and pins the router and selector for the lifetime of the deployment — there is no safe default, and no way to change the router or selector later:

   ```sh
   export PA_OWNER=<the owner's pubkey>
   export PA_VERIFIER_ROUTER=<verifier router program ID>
   export PA_PROOF_SELECTOR=<4-byte hex selector>
   ```

   Running `deploy pa` without them set prints the current devnet values, which are defined once in `scripts/validator-deploy.sh` and recorded in the cluster's deployment record.
3. Program keypairs present under `target/deploy/` (they are committed to git and restored automatically). Cluster deploys refuse to invent fresh program IDs; if you intend a new ID, generate keypairs with `./scripts/dev.sh anchor-build` and commit the ones you deploy.

Procedure:

```sh
./scripts/dev.sh deploy pa --cluster devnet        # or: deploy all; publishes the IDL, stops before init
# by hand: write the security metadata (below), then give both metadata accounts to the owner
./scripts/dev.sh init --cluster devnet             # hands the upgrade authority to the program
./scripts/dev.sh status --cluster devnet           # verify: deployed + PAState initialized
```

On devnet and mainnet, `deploy pa` publishes the IDL and stops before `initialize`. The program's canonical metadata accounts (Program Metadata program: `idl`, and `security` below) are created only by the program's upgrade authority, which `initialize` hands to the program. Before `init`, the deployer gives each of them to the owner, who signs every later update:

```sh
npx @solana-program/program-metadata@latest set-authority idl <PROGRAM_ID> --new-authority <owner> \
  --keypair <deployer keypair> --rpc <rpc>
npx @solana-program/program-metadata@latest set-authority security <PROGRAM_ID> --new-authority <owner> \
  --keypair <deployer keypair> --rpc <rpc>
```

On localnet, which has no Program Metadata program, `deploy pa` initializes at once.

`deploy` builds the production binary by default and verifies that `close_markers_batch` — a development-only instruction that deletes nullifier markers, i.e. replay protection — is absent from it. Passing `--dev-teardown` opts into the development build, which carries `close_markers_batch`; it is refused on every cluster but localnet, so a live deployment never has an instruction that deletes replay protection.

`idl-publish` stores the production IDL in the program's canonical Program Metadata IDL account on chain (Anchor 1.x `anchor idl upgrade`, which runs the `@solana-program/program-metadata` client through `npx`; devnet and mainnet only), so explorers and generic Anchor clients decode the deployment's instructions and events without out-of-band files. It rebuilds the production IDL (which self-checks that no dev-only instruction leaks into it), then verifies the cluster serves exactly the published file. Rerun it after every `upgrade` that changes the interface.

The program's display metadata — name, icon, description, project links, and the security contact (`security@anoma.foundation`, same as the EVM PA's `@custom:security-contact`) — lives in `docs/program-metadata.json` and is published to the program-metadata PDA that Solana Explorer reads:

```sh
npx @solana-program/program-metadata@latest write security <PROGRAM_ID> \
  docs/program-metadata.json --rpc <rpc-url> --keypair scripts/devnet-wallet.json
```

The first write, which creates the account, is signed by the program's upgrade authority, before `initialize`; later writes by the account's authority, the owner (above). Republish after changing the JSON. The logo URL is the public Anoma GitHub org avatar: this repository is private, so assets in it (`docs/assets/anoma-logo.jpeg`) are not fetchable by explorers — a publicly served URL is required.

After a first deployment, update `docs/DEVNET_DEPLOYMENT.md` (or the equivalent record for the cluster) with the program IDs, wallet, router, selector, and date.

To ship new code to an existing deployment: `./scripts/dev.sh upgrade --cluster <c>`, with the owner's wallet. It writes the new binary to a loader buffer the owner holds and calls the program's `upgrade` with it, which hands the buffer to the program's PDA and upgrades through the loader, announcing `UpgradedEvent` with the new code's executable hash (sha256 of the code without trailing zero bytes, what `solana-verify get-program-hash` reports); the command checks that the program then runs that code. While the wallet still holds the upgrade authority (before `initialize`, or before `migrate_state` below), it upgrades through the loader directly. Upgrading replaces the binary in place; it does not touch PAState, markers, or the initialization parameters. The new code runs from the next slot.

### Upgrades that change the state layout

`upgrade` replaces code only. The state account (`PAStateAccount`, PDA seed `pa_state`) keeps whatever bytes it had, so a binary whose account layout differs from the deployed one cannot read it. The adapter makes that failure explicit instead of accidental:

- Byte 8 of the account data (the first byte after Anchor's discriminator) is the **schema version**, written by `initialize` from `SCHEMA_VERSION` (programs/solana-pa-prototype/src/state.rs, exported in the IDL). It is a layout number and changes only when the layout does.
- Every instruction that reads the state account refuses it when byte 8 is not the binary's own version, `txdata_init` included. `txdata_write`, `txdata_close`, and `txdata_close_expired` never load `PAStateAccount`, so they keep working against a foreign version regardless of migration status, letting uploaders reclaim rent mid-migration. An upgrade to a layout-changing binary therefore stops the rest of the adapter cold until the account is migrated; nothing misreads old bytes.
- The refusal surfaces as one of two errors depending on the account's bytes. When the old account still deserializes under the new binary's layout — a version-byte mismatch only — the error is `UnsupportedStateSchema`. When it does not (a re-serialization shorter than an earlier one leaves stale bytes past its end, where a newer layout reads its appended fields), Anchor's `AccountDidNotDeserialize` surfaces first, because account deserialization runs before constraints. Either way the instruction is refused before it runs.

A release that changes the layout ships its migration with it, the counterpart of the call pa-evm's owner passes to `upgradeToAndCall`: an owner-only instruction that the owner runs once, right after upgrading the program in place and before any other instruction. It declares the state account unchecked (the typed layout cannot read it), requires this program as owner and the previous version at byte 8, parses the previous layout (never reinterpreting its bytes: a shorter re-serialization leaves stale bytes past its end), reallocates the account, and writes the new layout with the new version; the upgrade authority pays the rent difference. Only the version byte has a fixed offset (byte 8); a new layout may add, remove or reorder the other fields, which is why the migration parses the previous layout. The release that brings the migration also brings a test that starts from the previous build and runs the upgrade path: `tests/upgrade/<program>.ts`, on a validator of its own that loads the build in `tests/fixtures/previous/`.

This build's migration is `migrate_state`, from schema version 2, whose owner was the program's upgrade authority, to 3, which stores the owner. The upgrade authority signs it, pays for the account's 32 bytes of growth and becomes the stored owner; it hands the upgrade authority to the program's PDA and emits `OwnershipTransferredEvent` from the zero key, as `initialize` does. Since it hands over an authority, it is made by hand, after the upgrade (through the loader, the authority still being the wallet's) and after giving the canonical metadata accounts to the owner (Deploy and initialize). From then on the program upgrades only through `upgrade`, and the migration cannot run again.

This covers the state account only. A change to the commitment tree itself (hash, arity, leaf encoding) or to the marker PDA seeds invalidates the existing tree and marker addresses, and no in-place migration recovers that: it is a fresh deployment plus a bulk copy of roots and nullifiers.

The development build's `dev_set_schema_version` instruction exists only to test the refusal; the release build asserts it is absent, alongside `close_markers_batch`.

### Verified (reproducible) builds

The deployed PA should be the deterministic `solana-verify` Docker build, so the on-chain bytes are checkable against this repo:

```sh
./scripts/dev.sh verify-build --cluster devnet      # build + compare against deployed
./scripts/dev.sh upgrade pa --cluster devnet --prebuilt   # ship that exact artifact
```

`verify-build` needs solana-verify 0.5.2 (`cargo install solana-verify --version 0.5.2 --locked`). It builds with the pinned image (`[workspace.metadata.cli]` in `Cargo.toml` selects it — keep it in lockstep with `flake.nix`) for SBPF v3, the architecture every build of these programs targets, and fails loudly if the deployed program doesn't match. **A normal build overwrites the artifact with non-matching bytes** — after any `anchor-build`, `release-build`, `anchor-test`, `idl-publish`, or `deploy`/`upgrade` without `--prebuilt`, rerun `verify-build` before an upgrade you intend to keep verified. `verify-build` builds only the PA. To validate the artifact behaviorally before shipping, with every program's production binary in `target/deploy` (`release-build`, then `verify-build`) and the `PA_*` and `STF_*` initialization variables exported: `dev.sh validator` (backgrounded), `dev.sh deploy --cluster localnet --prebuilt`, `dev.sh anchor-test --cluster localnet --prebuilt`.

The deployment is then reproduced and checked with:

```sh
solana-verify verify-from-repo -u <rpc> --program-id <PROGRAM_ID> \
  https://github.com/anoma/solana-protocol-adapter --mount-path solana-pa-prototype \
  --library-name protocol_adapter --arch v3
```

**Reach depends on repo visibility.** This repository is private, so today only people with read access (their git credentials satisfy the clone) can run the check; the on-chain verification PDA points at a repo outsiders cannot fetch. The explorer "Verified" badge requires more on both axes: the OtterSec remote API serves mainnet only, and its worker must be able to clone the repo — i.e. the source (at least at the recorded commit) must be public. A mainnet deployment that should carry the badge therefore requires opening the source; that is a product decision, not an operational step.

## The settlement lookup table

Every settlement carries accounts that never change for a deployment: fifteen fixed ones (PAState, the system program, the verifier router, its router PDA and verifier entry, the verifier program, the event authority, the instructions and clock sysvars, the two forwarders, the SPL forwarder's config, event authority and escrow authority, and the SPL token program) plus each supported mint's escrow ATA. Submitters send settlements as v0 transactions against an address lookup table holding those keys, which costs one byte per key instead of 32 and keeps the first-wrap settlement (ed25519 authorization, inline bitmap init, settle) well inside the 1,232-byte packet.

```sh
./scripts/dev.sh lookup-table --cluster devnet                      # create
PA_LOOKUP_TABLE=<address> STF_TOKEN_MINTS=<mint>,<mint> \
  ./scripts/dev.sh lookup-table --cluster devnet                    # extend
```

The command derives the key set from the deployed programs and the PAState's pinned router and selector, so it runs after `deploy pa`. The signing wallet is the table's authority and stays so (the table is not frozen) because supporting a new mint means extending it. A table entry need not exist on chain: the forwarder's keys go in before the forwarder is deployed, and a mint's escrow ATA before `forwarder init` creates it. Extending is idempotent; a rerun adds only what is missing.

Record the address in the cluster's deployment record and ship it as `SETTLE_LOOKUP_TABLE` in anoma-pa-solana-client. A program-id rotation is a new deployment and gets a new table.

## The logic-ref denylist

The owner denies a logic ref for good, as pa-evm's owner does with `denyLogicRef`: from that slot on, no settlement consumes or creates a resource carrying it (`DeniedLogicRef`). It is the per-logic kill switch for a compromised resource logic, short of stopping the whole adapter.

```sh
PA_DENIED_LOGIC_REF=<hex, 32 bytes> ./scripts/dev.sh deny-logic-ref --cluster <c>   # owner wallet
```

The zero ref and a ref already denied are rejected; a denial cannot be undone. The denied refs are part of the state account (`denied_logic_refs`), which grows by 32 bytes per denial at the owner's expense, and each denial emits `LogicRefDeniedEvent`.

## The kind table

The PA stores the sha256 commitment of the kind table every settled aggregation instance must carry, and rejects a transaction proven against any other table. `initialize` installs the empty table's commitment (`e3b0c442…`, fixture-gen's committed `kind_table.json`) and emits `KindTableCommitmentUpdatedEvent` with it, as pa-evm's initializer does; the owner replaces it in place, as the EVM adapter's owner does with `setKindTableCommitment`:

```sh
PA_KIND_TABLE_COMMITMENT=<hex, 32 bytes> ./scripts/dev.sh set-kind-table --cluster <c>   # owner wallet
```

The instruction rejects a zero commitment and emits `KindTableCommitmentUpdatedEvent` with the new value, as pa-evm's `KindTableCommitmentUpdated` does. From that slot on, transactions proven against the previous table are rejected (`KindTableCommitmentMismatch`), so provers must load the new table before it is installed. The generated Solana tables and their commitments come from anoma/risc0-kind-tables (`crates/kind-tables/data/generated/<environment>/commitments.json`, keyed `solana:<genesis hash prefix>`; anoma/dos-pm#61).

## Pausing

A pause halts settlement, as pa-evm's owner does with `pause()`: for a suspected vulnerability, or while an upgrade that fixes one is prepared. `unpause` resumes it, as pa-evm's `unpause()` does.

```sh
./scripts/dev.sh pause --cluster devnet     # owner wallet
./scripts/dev.sh unpause --cluster devnet   # owner wallet
```

Both are owner-only. `pause` is refused while already paused (`EnforcedPause`) and `unpause` while not paused (`ExpectedPause`), as OpenZeppelin's Pausable refuses them; each emits `PausedEvent` / `UnpausedEvent` with the signer, as `Paused(account)` / `Unpaused(account)` do. The commands are idempotent: they do nothing when the adapter is already in the requested state.

What pauses: `settle` and `settle_from_txdata` reject every transaction with `EnforcedPause`. Those are the only two instructions gated on the flag, as `execute` is pa-evm's only `whenNotPaused` function.

What keeps working: everything else. All accounts (PAState, the commitment tree, nullifier and root markers) remain on chain and readable. Transaction-data upload accounts can still be closed and their rent reclaimed by their owners. Expiry configuration, the kind table, the denylist, upgrades, and moving the ownership still function.

A pause is as permanent as the ownership's custody: the owner can unpause. To make it permanent, finish with the Sunsetting steps below.

### The second kill switch: the verifier

Settlement also depends on RISC0's verifier router (the program pinned at `initialize`). The router keeps its own per-verifier emergency stop, checked inside the router during the verification call — the PA does not check it and cannot: a failed cross-program invocation aborts the whole transaction, so the PA never sees the error to translate it. As in pa-evm V2, the adapter reports the two separately: `PAStateAccount.paused` is its own pause, and the `risc_zero_verifier_paused` view (read by simulation, as pa-evm's `riscZeroVerifierPaused()`) reads the router's verifier entry for the deployment's selector. `initialize` refuses a verifier the router has already paused (`RiscZeroVerifierPaused`), as pa-evm's initializer does.

Diagnosis is by transaction logs: a router-side stop shows the router program's own `SelectorDeactivated` error in the failed transaction's logs, clearly distinct from a proof failure. Whether anyone outside this project can trip that switch depends on who owns the router deployment — the current owner is in the cluster's deployment record.

## Recovery

A pause is recovered in place: fix the code if needed (`upgrade`, then any migration), then `unpause`, as pa-evm's owner does with `upgradeToAndCall` and `unpause()`. The tree, markers and every resource stay valid.

When the deployment itself cannot be trusted any more, recovery is migration to a new deployment: deploy a fresh PA under a new program ID (new keypair), initialize it, and have applications re-establish their state against the new deployment's empty commitment tree. The old deployment's tree, markers, and history remain on chain and readable, so nothing about the old state is lost as evidence — but resources committed to the old tree cannot be settled in the new one, and value they represent must be recovered at the application layer (each application proves what it owned in the old tree and re-issues it in the new one, under whatever policy its owners decide).

The PAState account can never be re-initialized. This is deliberate: the account address derives from a fixed seed, so re-initializing would resurrect old nullifier-marker addresses and let previously spent notes spend again.

## The SPL token forwarder

The SPL token forwarder (`programs/spl-token-forwarder`) holds AnomaPay's wrapped SPL tokens in escrow and executes the wrap and unwrap calls the adapter forwards to it. It has two authorities, both recorded in its config PDA at initialization: the **owner** (The owner), which upgrades the program and rotates the logic ref, and the **emergency committee**, which names the emergency caller and closes accounts. All commands below go through `./scripts/dev.sh forwarder <command> --cluster <c>`; their parameters are `STF_*` environment variables (`scripts/forwarder.ts` lists them).

### Deploy and initialize

```sh
export STF_LOGIC_REF=<32-byte hex verifying key of the AnomaPay resource logic>
export STF_EMERGENCY_COMMITTEE=<base58 pubkey>
export STF_TOKEN_MINT=<base58 mint>          # optional: also creates the mint's escrow ATA
./scripts/dev.sh deploy stf --cluster devnet  # or: forwarder init, for an already deployed program
```

The program's upgrade authority, the deployer, initializes the config, as the EVM proxy runs its initializer at deployment; no other signer can. It hands the upgrade authority to the program's PDA. The config pins the adapter program id, the logic ref, the committee and the owner (`STF_OWNER`). A wrap is only executed when the adapter forwards it for a resource carrying that logic ref. One escrow authority, a PDA of the forwarder, owns every mint's escrow: the associated token account of the authority and the mint, as the EVM forwarder holds every token at its own address. `forwarder init` with `STF_TOKEN_MINT` creates a mint's escrow account, and the same command adds further mints later. Add each new mint's escrow account to the settlement lookup table as well (`lookup-table` with `STF_TOKEN_MINTS`).

### Nonce bitmaps

A wrap's replay protection is a per-user, per-256-nonce-word bitmap account. The adapter forwards no signer to the forwarder, so the forwarder cannot create that account during a wrap; the submitter creates it with the permissionless `init_nonce_bitmap` instruction (any payer) when the word's bitmap does not exist, and the wrap fails with `NonceBitmapMissing` when it is absent. The init fits in the settlement transaction itself, after the ed25519 instruction the wrap input points at; the integration suite settles the first wrap that way.

### Rotating the logic ref

The logic ref changes whenever the resource circuit is rebuilt. It is rotated as the EVM forwarder's is: the owner upgrades the proxy to an implementation whose `reinitializer(n)` writes the new ref. The forwarder's config records the version it was last initialized at; `reinitialize` writes the new ref only while that version is below the build's `CONFIG_VERSION`, and then records it, so each build rotates once. To rotate, raise `CONFIG_VERSION` by one in a new build, upgrade the program in place, and reinitialize, both with the owner's wallet:

```sh
./scripts/dev.sh upgrade stf --cluster <c>                                                             # owner wallet
STF_LOGIC_REF=<new 32-byte hex verifying key> ./scripts/dev.sh forwarder reinitialize --cluster <c>   # owner wallet
```

Escrow, nonce bitmaps and the committee are untouched. The instruction emits `Initialized` with the new version, as OpenZeppelin's reinitializer does; read the new ref from the config account. Resources wrapped under the previous ref leave through the new one once the adapter's kind table lists the previous version as an alias of the new one (anoma/risc0-kind-tables ADR-0008, rule R2): a transaction converts each into a resource under the new ref, which then unwraps. Until that table's commitment is installed (`set-kind-table`), they stay in escrow and can neither unwrap nor convert. The emergency path below is for a stopped adapter only.

### Checking a devnet deployment with a wrap and an unwrap

The integration suite's wraps run only on a fresh local deployment, so a devnet deployment, upgrade or logic-ref rotation is exercised end to end by hand, with fixture-gen's seeded test user and mint (their keys derive from public labels, so this is for devnet only) and anoma-pa-solana-client's `settle-fixture` tool:

1. Prove a wrap against the kind table the adapter stores, under a forwarder nonce the seeded user has not used on this deployment: `./scripts/dev.sh gen-fixtures spl-token-wrap --kind-table <table.json> --wrap-nonce <n> <wrap.json>`.
2. As the seeded user, mint the amount and approve the forwarder's escrow authority, then settle the wrap with `settle-fixture`.
3. Read the deployment's commitments in tree order from an indexer (the Envio project's created tags ordered by block, transaction index, action log index and tag index) into a JSON array of hex strings, and check that their root equals the adapter's on-chain root.
4. Prove the unwrap of the wrap's resource over that tree, `./scripts/dev.sh gen-fixtures spl-token-unwrap --kind-table <table.json> --wrap <wrap.json> --preceding-leaves <leaves.json> <unwrap.json>`, against the same kind table, where the leaves are the commitments before the wrap's, and settle it with `settle-fixture`.

### Upgrading the forwarder

The forwarder is upgraded in place, as the EVM forwarder's proxy is upgraded through `upgradeToAndCall`: the program id, the config, the escrow and the nonce bitmaps stay. The owner upgrades it as the adapter's (`dev.sh upgrade stf`). A release that changes an account layout ships owner-only migration instructions, the counterpart of the call the EVM owner passes to `upgradeToAndCall`, which run once, right after the upgrade, and a test that runs the upgrade path from the previous build (`tests/upgrade/spl_token_forwarder.ts`).

This build's migration is `migrate_config`, from the previous build's config, whose owner was the program's upgrade authority, to this layout, which stores the owner. The upgrade authority signs it, pays for the config's 32 bytes of growth and becomes the stored owner; it hands the upgrade authority to the program's PDA and emits `OwnershipTransferred` from the zero key. Like `migrate_state` it is made by hand, after the upgrade through the loader, and cannot run again. The escrow and the nonce bitmaps keep their layouts.

### Emergency committee

The committee and its emergency caller carry over the EVM V1 forwarder's emergency mechanism; the EVM V2 forwarder has none (it relies on its owner's upgrades), and anoma/dos-pm#86 tracks whether this forwarder keeps, replaces or drops it. As built:

Once the adapter is paused (`pause`), the committee names an emergency caller, once, and that caller withdraws from escrow directly without going through the adapter:

```sh
STF_TOKEN_MINT=<mint> STF_RECIPIENT=<owner> STF_AMOUNT=<raw units> \
  ./scripts/dev.sh forwarder emergency-withdraw --cluster <c>                                # caller wallet
```

Naming the emergency caller grants a key the right to withdraw escrowed funds, so, like every authority change on a live cluster, it is done by hand: no repository command or script builds it. The committee constructs and signs the forwarder's `set_emergency_caller(caller)` itself from the IDL (accounts: the committee as signer, the config, the paused adapter's PAState). The program refuses it while the adapter is not paused and refuses a second one. The forwarder's other committee instructions, `close_escrow` (drain an escrow to a recipient and close it), `close_nonce_bitmaps_batch` and `close_config`, refuse while the adapter is not paused, and are likewise made by hand.

### Retiring the forwarder

Retirement closes accounts for good, so on a live cluster it is done by hand (see The owner). Once the adapter is paused, the committee signs, in order: `close_nonce_bitmaps_batch` over every nonce bitmap the forwarder owns (closing them ends wrap replay protection, so the forwarder must not be initialized again afterwards), `close_escrow` for each mint's escrow (drains it to the committee's token account and closes it), then `close_config`, which also ends the forwarder's ownership: with no config, neither `upgrade` nor `reinitialize` can run. The program stays on chain, its upgrade authority its own PDA.

## Sunsetting

Permanent retirement, as pa-evm's owner retires its adapter, in order:

1. **Pause settlement.** `./scripts/dev.sh pause --cluster <c>`.
2. **Marker rent stays.** Nullifier and root markers are replay protection; no build deployed to a live cluster has an instruction that deletes them, so their rent is abandoned by design.
3. **Renounce the ownership, by hand** (The owner). No one can unpause, upgrade or reconfigure the adapter again. The paused program stays on chain and its state stays readable at its original addresses; its upgrade authority remains the program's PDA, which only `upgrade` could use, so the program can no longer be closed either.

