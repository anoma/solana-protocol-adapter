//! Shared creation for zero-data protocol marker PDAs.
//!
//! Markers record a fact by existing. Their addresses are publicly derivable, and
//! a plain transfer needs no signature from the recipient, so anyone can bring a
//! System-owned account into existence at a marker address for the rent-exempt
//! minimum. `system_instruction::create_account` refuses any address holding
//! lamports, which would let that block settlement permanently.
//!
//! Because a program-derived address is off-curve, only this program can
//! `allocate` or `assign` there. The only state an outside party can reach is
//! System-owned with zero data, which this primitive adopts: it tops up any
//! shortfall and assigns ownership. Markers hold no data, so no allocation is
//! needed. Every other state fails closed.

use crate::error::PAError;
use anchor_lang::prelude::*;
use anchor_lang::solana_program::program::invoke_signed;
use anchor_lang::system_program;
use solana_system_interface::instruction as system_instruction;

/// Create the marker account, adopting a System-owned empty placeholder if one is
/// already funded at that address.
///
/// The caller must have already checked that `marker` is at the expected derived
/// address, and must have resolved the case where it is owned by this program:
/// that means different things for nullifiers (replay) and roots (duplicate
/// root), so it is not decided here.
pub fn create_or_adopt_marker<'info>(
    program_id: &Pubkey,
    signer_seeds: &[&[u8]],
    payer: &AccountInfo<'info>,
    marker: &AccountInfo<'info>,
    system_program_account: &AccountInfo<'info>,
    lamports: u64,
) -> Result<()> {
    require!(
        marker.owner == &system_program::ID,
        PAError::MarkerUnexpectedOwner
    );
    require!(marker.data_is_empty(), PAError::MarkerUnexpectedData);

    let current = marker.lamports();

    if current == 0 {
        let ix = system_instruction::create_account(
            payer.key, marker.key, lamports, 0, // existence alone is the record
            program_id,
        );
        invoke_signed(
            &ix,
            &[
                payer.clone(),
                marker.clone(),
                system_program_account.clone(),
            ],
            &[signer_seeds],
        )?;
        return Ok(());
    }

    // Adopt the funded placeholder. The occupant's lamports become the marker's
    // rent; a balance above the minimum is kept and is reclaimable at teardown.
    if current < lamports {
        let ix = system_instruction::transfer(payer.key, marker.key, lamports - current);
        anchor_lang::solana_program::program::invoke(
            &ix,
            &[
                payer.clone(),
                marker.clone(),
                system_program_account.clone(),
            ],
        )?;
    }

    let ix = system_instruction::assign(marker.key, program_id);
    invoke_signed(
        &ix,
        &[marker.clone(), system_program_account.clone()],
        &[signer_seeds],
    )?;

    Ok(())
}
