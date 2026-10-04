# Program addresses and keypairs

Each program reads its address at compile time: its `declare_id!` takes the
variable `<NAME>_PROGRAM_ID` (`protocol_adapter` → `PROTOCOL_ADAPTER_PROGRAM_ID`).
The scripts export these variables from the files here:

- `localnet.env`: every program's address on the local validator, which loads
  each program at genesis at that address. The committed fixtures prove against
  these addresses.
- `<cluster>.env` (`devnet.env`, and `mainnet.env` from the first mainnet
  deploy): the address of every program deployed to that cluster. It is loaded
  on top of `localnet.env`, so the localnet-only programs keep their local
  addresses.

Program keypairs are never committed. A program's keypair is needed only to
create the program at its address, on its first deploy to a cluster; upgrades
go by address. Each operator names the keypair paths in the uncommitted
`<cluster>.keys.env`, one line per program:

```
PROTOCOL_ADAPTER_PROGRAM_KEYPAIR=/path/outside/the/repository/protocol_adapter-keypair.json
```

A deploy refuses a keypair whose address is not the one `<cluster>.env` gives
the program.

Changing an address in `localnet.env` invalidates the committed fixtures
(`./scripts/dev.sh regen-fixtures`) and, for the mock verifier, the VerifierEntry
genesis accounts (`scripts/regen-mock-verifier-entry.ts`), which the validator
checks before it starts.
