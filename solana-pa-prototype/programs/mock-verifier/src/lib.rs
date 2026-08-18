//! Mock Verifier — LOCALNET ONLY. Never deploy to a real cluster.
//!
//! Implements the risc0 verifier-router verifier interface (the same
//! `verify` instruction shape as `groth_16_verifier`) but performs no
//! cryptography: it accepts a "proof" iff `pi_c[..32]` equals the risc0
//! receipt-claim digest for `(image_id, journal_digest)`, computed with the
//! real verifier's `hash_claim`. This lets integration tests settle
//! transactions whose seals were produced without proving (RISC0 dev mode).
//!
//! The router dispatches to this program via a synthetic `VerifierEntry`
//! account (selector 0xffffffff) preloaded at test-validator genesis. A PA
//! instance only accepts these seals if it was initialized with that
//! selector; instances pinned to the Groth16 selector are unaffected.

use anchor_lang::prelude::*;
use anchor_lang::solana_program::log::sol_log_data;
use anchor_lang::system_program;
use groth_16_verifier::{hash_claim, Proof};

declare_id!("H3ZFoDHFvthGZu3kxpif3oSWm8MQn8uKvgDhrvVVHvHf");

#[derive(Accounts)]
// Can't be empty when CPI is enabled, see anchor #1628.
pub struct VerifyProof<'info> {
    /// CHECK: Only included to satisfy Anchor CPI requirements
    #[account(address = system_program::ID)]
    pub system_program: AccountInfo<'info>,
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
        let claim = hash_claim(&image_id, &journal_digest);
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
