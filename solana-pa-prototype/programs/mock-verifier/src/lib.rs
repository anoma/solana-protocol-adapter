//! Mock Verifier — LOCALNET ONLY. Never deploy to a real cluster.
//!
//! Implements the risc0 verifier-router verifier interface (the same
//! `verify` instruction shape as `groth_16_verifier`) but performs no
//! cryptography: it accepts a "proof" iff `pi_c[..32]` equals the risc0
//! receipt-claim digest for `(image_id, journal_digest)`, the claim the real
//! verifier's public inputs commit to. This lets integration tests settle
//! transactions whose seals were produced without proving (RISC0 dev mode).
//!
//! The router dispatches to this program via a synthetic `VerifierEntry`
//! account (selector 0xffffffff) preloaded at test-validator genesis. A PA
//! instance only accepts these seals if it was initialized with that
//! selector; instances pinned to the Groth16 selector are unaffected.

use anchor_lang::prelude::*;
use anchor_lang::solana_program::log::sol_log_data;
use anchor_lang::system_program;
use risc0_zkvm::sha::{Digest, Digestible};
use risc0_zkvm::{MaybePruned, ReceiptClaim};

declare_id!("H3ZFoDHFvthGZu3kxpif3oSWm8MQn8uKvgDhrvVVHvHf");

// The `Proof` the router passes to a verifier's `verify`, from the router's
// IDL (see the adapter's `declare_program!(verifier_router)`).
declare_program!(verifier_router);
use verifier_router::types::Proof;

/// Digest of the receipt claim of a successful (halted, exit code 0) run of
/// `image_id` whose journal hashes to `journal_digest`, with no input and no
/// assumptions.
pub fn claim_digest(image_id: &[u8; 32], journal_digest: &[u8; 32]) -> [u8; 32] {
    let claim = ReceiptClaim::ok(
        Digest::from(*image_id),
        MaybePruned::Pruned(Digest::from(*journal_digest)),
    );
    claim.digest().into()
}

#[derive(Accounts)]
// Can't be empty when CPI is enabled, see anchor #1628.
pub struct VerifyProof<'info> {
    /// CHECK: Only included to satisfy Anchor CPI requirements
    #[account(address = system_program::ID)]
    pub system_program: UncheckedAccount<'info>,
}

#[program]
pub mod mock_verifier {
    use super::*;

    /// Accepts the proof iff `pi_c[..32]` equals the receipt-claim digest
    /// for `(image_id, journal_digest)`.
    ///
    /// The digest rides in `pi_c` because the PA negates `pi_a` before the
    /// router CPI; `pi_c` reaches the verifier unmodified.
    pub fn verify(
        _ctx: Context<VerifyProof>,
        proof: Proof,
        image_id: [u8; 32],
        journal_digest: [u8; 32],
    ) -> Result<()> {
        let claim = claim_digest(&image_id, &journal_digest);
        if proof.pi_c[..32] != claim {
            // Base64 payloads in the tx log: expected claim digest, then
            // the seal's pi_c[..32].
            sol_log_data(&[&claim, &proof.pi_c[..32]]);
            return err!(MockVerifierError::ClaimDigestMismatch);
        }
        Ok(())
    }
}

// Offset keeps these codes disjoint from groth_16_verifier's (6000): the
// tamper tests assert which verifier rejected the seal by error code.
#[error_code(offset = 6600)]
pub enum MockVerifierError {
    #[msg("mock seal claim digest mismatch")]
    ClaimDigestMismatch,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `groth_16_verifier::hash_claim(&[0x11; 32], &[0x22; 32])` from
    /// risc0-solana v3.0.0 (commit ee415935): the claim digest the deployed
    /// Groth16 verifier derives its public inputs from.
    const RISC0_SOLANA_HASH_CLAIM: [u8; 32] = [
        0xb3, 0xcb, 0x89, 0x7e, 0xa4, 0xd0, 0xbd, 0x2a, 0x01, 0xc4, 0x19, 0x0c, 0xdc, 0x44, 0xf0,
        0x18, 0x52, 0x33, 0x01, 0x0a, 0x23, 0x80, 0xde, 0x19, 0x5e, 0x41, 0x04, 0x16, 0xb2, 0x43,
        0x7e, 0xc8,
    ];

    #[test]
    fn claim_digest_matches_risc0_solana_hash_claim() {
        assert_eq!(
            claim_digest(&[0x11; 32], &[0x22; 32]),
            RISC0_SOLANA_HASH_CLAIM,
            "the mock must accept exactly the claim digest the real verifier checks"
        );
    }
}
