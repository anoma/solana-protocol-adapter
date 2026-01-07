# Open Issues

## 1. `deletion_criterion` has no defined semantics

**Status:** Needs cross-repo discussion

**Location:**
- Defined: `arm_core/src/logic_instance.rs:47-51` — `ExpirableBlob.deletion_criterion: u32`
- Consumed: `solana-pa-prototype/src/lib.rs:968,977` — `DELETION_CRITERION_NEVER = 1`
- Produced: `arm_tests/arm_test_witness/src/lib.rs:40,44,59,67` — hardcoded `1`
- Produced: `solana-pa-prototype/src/external_calls.rs:19` and `tools/fixture-gen/src/main.rs:195` — hardcoded `0`

**Problem:**

`ExpirableBlob.deletion_criterion` is a bare `u32` with no enum, no named constants, and no documentation beyond "The deletion criterion for the blob." Two magic values are used across repositories:

- `0` = ephemeral (CPI call payloads, not persisted as events)
- `1` = "never delete" (resources, nullifier keys, ciphertexts, application data — persisted as Solana log events)

The Solana PA uses this field to decide whether to emit on-chain events: if `deletion_criterion == 1`, the payload is emitted as a log event; otherwise it is skipped (`lib.rs:977`). The arm-risc0 test witness hardcodes `1` for all persistent data. Neither repository defines what the values mean or whether more values are expected.

**Risks:**

- If arm-risc0 introduces a `DeletionCriterion` enum with different variant orderings, the PA's hardcoded `1` silently diverges.
- If new values are added (e.g., `2` = "delete after N blocks"), the PA would silently skip them since it only checks `== 1`.
- The field might effectively be a boolean, in which case `u32` is misleading.

**Recommendation:**

Define named constants or an enum in `arm_core` so both repositories share a single source of truth. At minimum: `pub const DELETION_CRITERION_IMMEDIATELY: u32 = 0;` and `pub const DELETION_CRITERION_NEVER: u32 = 1;`. If more values are planned, an enum is more appropriate.

**Affects:** `arm_core` (anoma/arm-risc0), `solana-pa-prototype` (anoma/solana-protocol-adapter), and any future PA implementations.
