# Devnet Deployment Record

**Deployed:** 2026-01-22
**Network:** Solana Devnet

## Deployed Programs

### Protocol Adapter (PA)
- **Program ID:** `AV1dFJCfq6CmJ523ft8YwNsQVYEoDxzNfEhEFJzoUkjt`
- **Authority:** `FZjHgvuQsgKYRBnYWDAvFKP9L9MJpEnh4vgDcKFSWtGh`

### Verifier Router
- **Program ID:** `CnhgPbCm2mjYYT2konzKsBD7RL8Mfg63nuzB7xsbABFq`
- **Router PDA:** `5GzjEjtL3JqKSp8sx4Kjzxuwg6xQ3pPyuTqnQJxbemEk`
- **Authority:** `FZjHgvuQsgKYRBnYWDAvFKP9L9MJpEnh4vgDcKFSWtGh`

### Groth16 Verifier
- **Program ID:** `DBcDFEFD87rLdoepucSxbvG13idCo6HYS4sutVihkmbk`
- **Selectors:**
  - `0x00000001` (generic)
  - `0x73c457ba` (matches fixture verifier_parameters)
- **Authority:** `5GzjEjtL3JqKSp8sx4Kjzxuwg6xQ3pPyuTqnQJxbemEk` (Router PDA)

## Architecture

```
┌─────────────────────┐
│  Protocol Adapter   │
│  (PA Program)       │
│  AV1dFJCfq...       │
└─────────┬───────────┘
          │ CPI (verify)
          ▼
┌─────────────────────┐
│  Verifier Router    │
│  CnhgPbCm...        │
│  ┌───────────────┐  │
│  │  Router PDA   │  │
│  │  5GzjEjt...   │  │
│  │  - owner      │  │
│  │  - verifiers  │  │
│  └───────────────┘  │
└─────────┬───────────┘
          │ Routes by selector
          ▼
┌─────────────────────┐
│  Groth16 Verifier   │
│  DBcDFEFD...        │
│  selector: 0x01     │
└─────────────────────┘
```

## Deployment Wallet

- **Address:** `FZjHgvuQsgKYRBnYWDAvFKP9L9MJpEnh4vgDcKFSWtGh`
- **Keypair:** `solana-pa-prototype/scripts/devnet-wallet.json`

## Usage Notes

1. The PA is configured to verify proofs via the Verifier Router
2. The Router looks up verifiers by their 4-byte selector
3. For Groth16 proofs, use selector `0x00000001`
4. The Router PDA owns the Groth16 Verifier (can close/estop it)

## Verifying Deployment

```bash
# Check programs exist
solana program show --url devnet AV1dFJCfq6CmJ523ft8YwNsQVYEoDxzNfEhEFJzoUkjt
solana program show --url devnet CnhgPbCm2mjYYT2konzKsBD7RL8Mfg63nuzB7xsbABFq
solana program show --url devnet DBcDFEFD87rLdoepucSxbvG13idCo6HYS4sutVihkmbk
```
