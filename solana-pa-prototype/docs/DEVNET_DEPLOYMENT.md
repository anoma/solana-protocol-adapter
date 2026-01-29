# Devnet Deployment Record

**Deployed:** 2026-01-26
**Network:** Solana Devnet

## Deployed Programs

### Protocol Adapter (PA)
- **Program ID:** `AV1dFJCfq6CmJ523ft8YwNsQVYEoDxzNfEhEFJzoUkjt`
- **Authority:** `FZjHgvuQsgKYRBnYWDAvFKP9L9MJpEnh4vgDcKFSWtGh`

### Verifier Router (RISC0 v3.0.0)
- **Program ID:** `BetEAE4npinksQBxvqUN1KkCVjYFJywWao45MSWtp5yg`
- **Router PDA:** `9ZJmYSYaYq38GfwQMsEw5gkzfr94Vbzw6Nv53yQuCv2S`
- **Authority:** `FZjHgvuQsgKYRBnYWDAvFKP9L9MJpEnh4vgDcKFSWtGh`

### Groth16 Verifier (RISC0 v3.0.0)
- **Program ID:** `2Yfa83Lzbn71ie3J1KQRiNQz1qHnvVm8gkBCpXZQ7ajD`
- **Verifier Entry PDA:** `4ktbrXwBXZMoND5qb3J6abS1m8KqwUtCjjDBebJ4vqey`
- **Selector:** `0x73c457ba` (matches fixture verifier_parameters)
- **Authority:** `9ZJmYSYaYq38GfwQMsEw5gkzfr94Vbzw6Nv53yQuCv2S` (Router PDA)

### Block Time Forwarder
- **Program ID:** `FLh2rbnAbtFZkLMMX36Fh4rV9wJWUFrLw5gDmoLzPEgq`
- **Purpose:** External call forwarder for time comparisons (required by fixture)

## Architecture

```
                    ┌─────────────────────┐
                    │  Protocol Adapter   │
                    │  (PA Program)       │
                    │  AV1dFJCfq...       │
                    └─────────┬───────────┘
                              │
          ┌───────────────────┼───────────────────┐
          │ CPI (verify)      │                   │ CPI (external calls)
          ▼                   │                   ▼
┌─────────────────────┐       │       ┌─────────────────────┐
│  Verifier Router    │       │       │  Block Time         │
│  BetEAE4n...        │       │       │  Forwarder          │
│  ┌───────────────┐  │       │       │  FLh2rbn...         │
│  │  Router PDA   │  │       │       └─────────────────────┘
│  │  9ZJmYSY...   │  │       │
│  │  - owner      │  │       │
│  │  - verifiers  │  │       │
│  └───────────────┘  │       │
└─────────┬───────────┘       │
          │ Routes by selector│
          ▼                   │
┌─────────────────────┐       │
│  Groth16 Verifier   │       │
│  2Yfa83L...         │       │
│  selector: 0x73c4.. │       │
└─────────────────────┘       │
```

## Deployment Wallet

- **Address:** `FZjHgvuQsgKYRBnYWDAvFKP9L9MJpEnh4vgDcKFSWtGh`
- **Keypair:** `solana-pa-prototype/scripts/devnet-wallet.json`

## Usage Notes

1. The PA is configured to verify proofs via the Verifier Router
2. The Router looks up verifiers by their 4-byte selector
3. For Groth16 proofs, use selector `0x73c457ba`
4. The Router PDA owns the Groth16 Verifier (can close/estop it)
5. These programs are cloned to localnet for testing via Anchor.toml

## Verifying Deployment

```bash
# Check programs exist
solana program show --url devnet AV1dFJCfq6CmJ523ft8YwNsQVYEoDxzNfEhEFJzoUkjt
solana program show --url devnet BetEAE4npinksQBxvqUN1KkCVjYFJywWao45MSWtp5yg
solana program show --url devnet 2Yfa83Lzbn71ie3J1KQRiNQz1qHnvVm8gkBCpXZQ7ajD
```
