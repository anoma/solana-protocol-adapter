# Operating a Protocol Adapter Deployment

This document is the operator's procedure set for a Protocol Adapter (PA) deployment: how to deploy and initialize it, upgrade it, run it, pause it, and retire it permanently. The one fact that shapes everything here: **the adapter is upgradeable, so "paused forever" is not enforced by the chain — it is enforced by custody of the upgrade authority.** The same holds for pa-evm V2, a UUPS proxy its owner upgrades in place (`upgradeToAndCall`) and pauses and unpauses (`pause()`, `unpause()`); the Solana adapter has the same owner-only `pause` and `unpause`. Burning the upgrade authority, the last Sunsetting step, makes a pause permanent.

Commands run through `./scripts/dev.sh` from `solana-pa-prototype/`, which enters the Nix shell automatically; cluster operations take `--cluster <localnet|devnet|mainnet>` (see `scripts/ops.sh` for all flags). devnet and mainnet operations go through the operator's RPC provider: pass `--url <rpc>` or set `DEVNET_RPC_URL` / `MAINNET_RPC_URL`; there is no public-endpoint default. Commands shown as `solana`, `npx` or `solana-verify` run directly.

`dev.sh anchor-test --cluster <devnet|mainnet>` runs a test subset against the live deployment. Its wallet must own nothing under test: the run refuses a wallet that is the upgrade authority of any deployed program, because a spec signing as the owner could pause the deployment, replace its kind table or renounce the authority, which makes the program final for good. Use a separate funded test wallet (`--wallet`).

## The owner

A deployment has one owner: each program's upgrade authority, which the BPF loader records in the program's ProgramData account. The adapter's owner-only instructions (`initialize`, `pause`, `unpause`, `update_expiry_config`, `set_kind_table_commitment`, `deny_logic_ref`, `migrate_state`) and the forwarder's (`initialize`, `reinitialize`, the `migrate_*` instructions) require it as signer. This mirrors pa-evm, whose owner is also the one who authorizes its upgrades.

Ownership moves with the upgrade authority, and is given up with it:

```sh
solana program set-upgrade-authority <program id> --new-upgrade-authority <new authority keypair> \
  --upgrade-authority <current authority keypair> --url <rpc>                              # moves ownership, at once
solana program set-upgrade-authority <program id> --final \
  --upgrade-authority <current authority keypair> --url <rpc>                              # renounces it for good
```

These are typed by hand: no repository command or script changes an authority on a live cluster, and the only builders of authority instructions in the repository (`tests/utils/localOnly.ts`) refuse any endpoint but a local validator. The new authority must sign unless `--skip-new-upgrade-authority-signer-check` is passed (for a key that cannot sign, such as a multisig vault). No program instruction or event is involved; the loader's ProgramData account is where the current owner is read. A final program can never be upgraded, initialized again, paused, unpaused or reconfigured.

The owner can replace the program binary, which means it could deploy code that undoes a stop: a stop is only as permanent as upgrade-authority custody. Current key custody per cluster lives in the deployment record (`docs/DEVNET_DEPLOYMENT.md` for devnet), which is updated after every operation.

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

`idl-publish` stores the production IDL in the program's canonical Program Metadata IDL account on chain (Anchor 1.x `anchor idl upgrade`, which runs the `@solana-program/program-metadata` client through `npx`; devnet and mainnet only), so explorers and generic Anchor clients decode the deployment's instructions and events without out-of-band files. It rebuilds the production IDL (which self-checks that no dev-only instruction leaks into it), then verifies the cluster serves exactly the published file. Rerun it after every `upgrade` that changes the interface.

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

- Byte 8 of the account data (the first byte after Anchor's discriminator) is the **schema version**, written by `initialize` from `SCHEMA_VERSION` (programs/solana-pa-prototype/src/state.rs, exported in the IDL). It is a layout number and changes only when the layout does.
- Every instruction that reads the state account refuses it when byte 8 is not the binary's own version, `txdata_init` included. `txdata_write`, `txdata_close`, and `txdata_close_expired` never load `PAStateAccount`, so they keep working against a foreign version regardless of migration status, letting uploaders reclaim rent mid-migration. An upgrade to a layout-changing binary therefore stops the rest of the adapter cold until the account is migrated; nothing misreads old bytes.
- The refusal surfaces as one of two errors depending on the account's bytes. When the old account still deserializes under the new binary's layout — a version-byte mismatch only — the error is `UnsupportedStateSchema`. When it does not (a re-serialization shorter than an earlier one leaves stale bytes past its end, where a newer layout reads its appended fields), Anchor's `AccountDidNotDeserialize` surfaces first, because account deserialization runs before constraints. Either way the instruction is refused before it runs.

A release that changes the layout ships the migration with it, the counterpart of the call pa-evm's owner passes to `upgradeToAndCall`. The procedure is: upgrade the binary, then run its `migrate_state` once, signed by the upgrade authority, before any other instruction:

```sh
./scripts/dev.sh upgrade pa --cluster <c>          # upgrade-authority wallet
./scripts/dev.sh migrate-state --cluster <c>       # upgrade-authority wallet; idempotent
```

`migrate_state` declares the account unchecked (the typed layout cannot read it), requires this program as owner and the previous version at byte 8, parses the previous layout (never reinterpreting its bytes: a shorter re-serialization leaves stale bytes past its end), reallocates the account to the new size, zeroes it and writes the new layout with the new version. The upgrade authority pays the rent difference. Only the version byte has a fixed offset (byte 8); a new layout may add, remove or reorder the other fields, which is why `migrate_state` parses the previous layout instead of reading it in place.

Schema version 2 drops the stored authority and pending authority (the owner is the upgrade authority) and appends the logic-ref denylist (below); its `migrate_state` migrates from version 1, the layout of the build deployed on devnet. `tests/adapter-upgrade.ts` runs the whole path from that build (`tests/fixtures/previous/protocol_adapter.so`): settle through it, upgrade in place, migrate through `migrate-state`, settle again.

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

Every settlement carries accounts that never change for a deployment: fourteen fixed ones (PAState, the system program, the verifier router, its router PDA and verifier entry, the verifier program, the event authority, the instructions and clock sysvars, the two forwarders, the SPL forwarder's config and escrow authority, and the SPL token program) plus each supported mint's escrow ATA. Submitters send settlements as v0 transactions against an address lookup table holding those keys, which costs one byte per key instead of 32 and keeps the first-wrap settlement (ed25519 authorization, inline bitmap init, settle) well inside the 1,232-byte packet.

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
PA_DENIED_LOGIC_REF=<hex, 32 bytes> ./scripts/dev.sh deny-logic-ref --cluster <c>   # upgrade-authority wallet
```

The zero ref and a ref already denied are rejected; a denial cannot be undone. The denied refs are part of the state account (`denied_logic_refs`), which grows by 32 bytes per denial at the owner's expense, and each denial emits `LogicRefDeniedEvent`.

## The kind table

The PA stores the sha256 commitment of the kind table every settled aggregation instance must carry, and rejects a transaction proven against any other table. `initialize` installs the empty table's commitment (`e3b0c442…`, fixture-gen's committed `kind_table.json`) and emits `KindTableCommitmentUpdatedEvent` with it, as pa-evm's initializer does; the owner replaces it in place, as the EVM adapter's owner does with `setKindTableCommitment`:

```sh
PA_KIND_TABLE_COMMITMENT=<hex, 32 bytes> ./scripts/dev.sh set-kind-table --cluster <c>   # upgrade-authority wallet
```

The instruction rejects a zero commitment and emits `KindTableCommitmentUpdatedEvent` with the new value, as pa-evm's `KindTableCommitmentUpdated` does. From that slot on, transactions proven against the previous table are rejected (`KindTableCommitmentMismatch`), so provers must load the new table before it is installed. The generated Solana tables and their commitments come from anoma/risc0-kind-tables (`crates/kind-tables/data/generated/<environment>/commitments.json`, keyed `solana:<genesis hash prefix>`; anoma/dos-pm#61).

## Pausing

A pause halts settlement, as pa-evm's owner does with `pause()`: for a suspected vulnerability, or while an upgrade that fixes one is prepared. `unpause` resumes it, as pa-evm's `unpause()` does.

```sh
./scripts/dev.sh pause --cluster devnet     # upgrade-authority wallet
./scripts/dev.sh unpause --cluster devnet   # upgrade-authority wallet
```

Both are owner-only. `pause` is refused while already paused (`EnforcedPause`) and `unpause` while not paused (`ExpectedPause`), as OpenZeppelin's Pausable refuses them; each emits `PausedEvent` / `UnpausedEvent` with the signer, as `Paused(account)` / `Unpaused(account)` do. The commands are idempotent: they do nothing when the adapter is already in the requested state.

What pauses: `settle` and `settle_from_txdata` reject every transaction with `EnforcedPause`. Those are the only two instructions gated on the flag, as `execute` is pa-evm's only `whenNotPaused` function.

What keeps working: everything else. All accounts (PAState, the commitment tree, nullifier and root markers) remain on chain and readable. Transaction-data upload accounts can still be closed and their rent reclaimed by their owners. Expiry configuration, the kind table, the denylist, upgrades, and moving the upgrade authority (`solana program set-upgrade-authority`) still function.

A pause is as permanent as upgrade-authority custody: whoever holds the upgrade authority can unpause. To make it permanent, finish with the Sunsetting steps below.

### The second kill switch: the verifier

Settlement also depends on RISC0's verifier router (the program pinned at `initialize`). The router keeps its own per-verifier emergency stop, checked inside the router during the verification call — the PA does not check it and cannot: a failed cross-program invocation aborts the whole transaction, so the PA never sees the error to translate it. As in pa-evm V2, the adapter reports the two separately: `PAStateAccount.paused` is its own pause, and the `risc_zero_verifier_paused` view (read by simulation, as pa-evm's `riscZeroVerifierPaused()`) reads the router's verifier entry for the deployment's selector. `initialize` refuses a verifier the router has already paused (`RiscZeroVerifierPaused`), as pa-evm's initializer does.

Diagnosis is by transaction logs: a router-side stop shows the router program's own `SelectorDeactivated` error in the failed transaction's logs, clearly distinct from a proof failure. Whether anyone outside this project can trip that switch depends on who owns the router deployment — the current owner is in the cluster's deployment record.

## Recovery

A pause is recovered in place: fix the code if needed (`upgrade`, then any migration), then `unpause`, as pa-evm's owner does with `upgradeToAndCall` and `unpause()`. The tree, markers and every resource stay valid.

When the deployment itself cannot be trusted any more, recovery is migration to a new deployment: deploy a fresh PA under a new program ID (new keypair), initialize it, and have applications re-establish their state against the new deployment's empty commitment tree. The old deployment's tree, markers, and history remain on chain and readable, so nothing about the old state is lost as evidence — but resources committed to the old tree cannot be settled in the new one, and value they represent must be recovered at the application layer (each application proves what it owned in the old tree and re-issues it in the new one, under whatever policy its owners decide).

The PAState account can never be re-initialized. This is deliberate: the account address derives from a fixed seed, so re-initializing would resurrect old nullifier-marker addresses and let previously spent notes spend again.

## The SPL token forwarder

The SPL token forwarder (`programs/spl-token-forwarder`) holds AnomaPay's wrapped SPL tokens in escrow and executes the wrap and unwrap calls the adapter forwards to it. It has two authorities: the program's **upgrade authority**, which upgrades the program and rotates the logic ref, and the **emergency committee**, recorded in its config PDA at initialization, which names the emergency caller and closes accounts. All commands below go through `./scripts/dev.sh forwarder <command> --cluster <c>`; their parameters are `STF_*` environment variables (`scripts/forwarder.ts` lists them).

### Deploy and initialize

```sh
export STF_LOGIC_REF=<32-byte hex verifying key of the AnomaPay resource logic>
export STF_EMERGENCY_COMMITTEE=<base58 pubkey>
export STF_TOKEN_MINT=<base58 mint>          # optional: also creates the mint's escrow ATA
./scripts/dev.sh deploy stf --cluster devnet  # or: forwarder init, for an already deployed program
```

The program's upgrade authority initializes the config, as the EVM proxy runs its initializer at deployment; no other signer can. The config pins the adapter program id, the logic ref, and the committee. A wrap is only executed when the adapter forwards it for a resource carrying that logic ref. One escrow authority, a PDA of the forwarder, owns every mint's escrow: the associated token account of the authority and the mint, as the EVM forwarder holds every token at its own address. `forwarder init` with `STF_TOKEN_MINT` creates a mint's escrow account, and the same command adds further mints later. Add each new mint's escrow account to the settlement lookup table as well (`lookup-table` with `STF_TOKEN_MINTS`).

### Nonce bitmaps

A wrap's replay protection is a per-user, per-256-nonce-word bitmap account. The adapter forwards no signer to the forwarder, so the forwarder cannot create that account during a wrap; the submitter creates it with the permissionless `init_nonce_bitmap` instruction (any payer) when the word's bitmap does not exist, and the wrap fails with `NonceBitmapMissing` when it is absent. The init fits in the settlement transaction itself, after the ed25519 instruction the wrap input points at; the integration suite settles the first wrap that way.

### Rotating the logic ref

The logic ref changes whenever the resource circuit is rebuilt. It is rotated as the EVM forwarder's is: the owner upgrades the proxy to an implementation whose `reinitializer(n)` writes the new ref. The forwarder's config records the version it was last initialized at; `reinitialize` writes the new ref only while that version is below the build's `CONFIG_VERSION`, and then records it, so each build rotates once. To rotate, raise `CONFIG_VERSION` by one in a new build, upgrade the program in place, and reinitialize with the upgrade-authority wallet:

```sh
./scripts/dev.sh upgrade stf --cluster <c>                                                             # upgrade-authority wallet
STF_LOGIC_REF=<new 32-byte hex verifying key> ./scripts/dev.sh forwarder reinitialize --cluster <c>   # upgrade-authority wallet
```

Escrow, nonce bitmaps and the committee are untouched. The instruction emits `Initialized` with the new version, as OpenZeppelin's reinitializer does; read the new ref from the config account. Resources wrapped under the previous ref leave through the new one once the adapter's kind table lists the previous version as an alias of the new one (anoma/risc0-kind-tables ADR-0008, rule R2): a transaction converts each into a resource under the new ref, which then unwraps. Until that table's commitment is installed (`set-kind-table`), they stay in escrow and can neither unwrap nor convert. The emergency path below is for a stopped adapter only.

### Upgrading the forwarder

The forwarder is upgraded in place, as the EVM forwarder's proxy is upgraded through `upgradeToAndCall`: the program id, the config, the escrow and the nonce bitmaps stay. A release that changes an account layout ships `migrate_*` instructions, the counterpart of the call the EVM owner passes to `upgradeToAndCall`, which the upgrade authority runs once, right after the upgrade:

```sh
./scripts/dev.sh upgrade stf --cluster <c>                                          # upgrade-authority wallet
STF_TOKEN_MINTS=<mint>[,<mint>...] ./scripts/dev.sh forwarder migrate --cluster <c>  # upgrade-authority wallet
```

This build's migrations bring the previous build's accounts to its layout:

- The config drops the bump the previous build stored after its fields (this build derives the config address at compile time) and records version 1, so this build's `reinitialize` can rotate its logic ref once.
- Each nonce bitmap keeps its bits and gains its canonical bump. The command finds every bitmap still in the previous layout by itself, reading its user and word from the `init_nonce_bitmap` that created it.
- Each listed mint's escrow moves from that mint's own authority (`["escrow", mint]`) to the one escrow authority, and the previous escrow account closes. The mints must be listed: escrow token accounts belong to the token program, not the forwarder, so the forwarder cannot enumerate them.

The command is idempotent. Until a bitmap is migrated, wraps on its word fail with `NonceBitmapMissing`; until a mint's escrow is migrated, its unwraps fail for lack of funds in the new escrow. Nothing misreads the old bytes. `tests/forwarder-upgrade.ts` runs this path from the previous build (`tests/fixtures/previous/spl_token_forwarder.so`).

### Emergency committee

The committee and its emergency caller carry over the EVM V1 forwarder's emergency mechanism; the EVM V2 forwarder has none (it relies on its owner's upgrades), and anoma/dos-pm#86 tracks whether this forwarder keeps, replaces or drops it. As built:

Once the adapter is paused (`pause`), the committee names an emergency caller, once, and that caller withdraws from escrow directly without going through the adapter:

```sh
STF_TOKEN_MINT=<mint> STF_RECIPIENT=<owner> STF_AMOUNT=<raw units> \
  ./scripts/dev.sh forwarder emergency-withdraw --cluster <c>                                # caller wallet
```

Naming the emergency caller grants a key the right to withdraw escrowed funds, so, like every authority change on a live cluster, it is done by hand: no repository command or script builds it. The committee constructs and signs the forwarder's `set_emergency_caller(caller)` itself from the IDL (accounts: the committee as signer, the config, the paused adapter's PAState). The program refuses it while the adapter is not paused and refuses a second one. The only builders of authority instructions in the repository are the tests' (`tests/utils/localOnly.ts`), which refuse any endpoint but a local validator. The committee can also drain and close an escrow outright with `drain-escrow`, but like every committee teardown command it refuses while the adapter is not paused.

### Retiring the forwarder

Once the adapter is paused, `STF_TOKEN_MINT=<mint> ./scripts/dev.sh forwarder teardown --cluster <c>` (committee wallet) closes every nonce bitmap, drains and closes that mint's escrow to the committee, and closes the config, reclaiming their rent. Run it once per mint that has an escrow, then close the program with `teardown stf` (which first attempts `close-pdas` on the adapter, as every `teardown` does).

## Sunsetting

Permanent retirement, in order. The order matters because `initialize` requires a live upgrade authority: once the authority is gone, that program ID can never host a PA again — which is the point, but only as the final step.

1. **Pause settlement.** `./scripts/dev.sh pause --cluster <c>`.
2. **Reclaim marker rent — development builds only.** `./scripts/dev.sh close-pdas --cluster <c>` closes nullifier and root markers via `close_markers_batch`, which requires a paused adapter and exists only in `--dev-teardown` builds. Production builds abandon marker rent by design; there is deliberately no production path that deletes replay-protection markers. The command validates the instruction against the locally built IDL, so run a development build first (`./scripts/dev.sh run "./scripts/ops.sh build-dev"`) if the last build was a production one.
3. **End the program.** Two mutually exclusive options:
   - `solana program close <PROGRAM_ID> --bypass-warning --keypair <upgrade authority keypair> --url <rpc>` — reclaims the program account's rent and burns the program ID permanently (`./scripts/dev.sh teardown --cluster <c>` does steps 2 and 3 together), or
   - `solana program set-upgrade-authority <PROGRAM_ID> --final --upgrade-authority <upgrade authority keypair> --url <rpc>` — keeps the paused program on chain forever but makes it immutable: no one can unpause or upgrade it, and the state remains readable at its original addresses.

For a beta deployment on devnet, closing the program (reclaiming rent) is the normal end. Immutability-by-burned-authority is the shape a mainnet retirement would take when the historical state should stay served at its known addresses.

