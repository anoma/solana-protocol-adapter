//! A program that owns its own upgrades, as a UUPS implementation does: the
//! loader's upgrade authority is a PDA of the program, so the loader accepts
//! an upgrade (or an authority change) only when the program signs for it,
//! which it does only for its owner. Shared by the adapter and the SPL token
//! forwarder, whose owner-only `upgrade` instructions wrap `upgrade_program`.

use anchor_lang::prelude::*;
use anchor_lang::solana_program::bpf_loader_upgradeable::{
    set_upgrade_authority_checked, upgrade, UpgradeableLoaderState,
};
use anchor_lang::solana_program::program::invoke_signed;

use crate::error::PAError;

/// Hand `program_id`'s upgrade authority from `current_authority` (a signer)
/// to its PDA at `[seed]`, which signs for itself here: the loader's checked
/// authority change requires both signatures.
pub fn hand_upgrade_authority_to_program<'info>(
    program_id: &Pubkey,
    seed: &[u8],
    bump: u8,
    program_data: &AccountInfo<'info>,
    current_authority: &AccountInfo<'info>,
    upgrade_authority: &AccountInfo<'info>,
    loader: &AccountInfo<'info>,
) -> Result<()> {
    invoke_signed(
        &set_upgrade_authority_checked(program_id, current_authority.key, upgrade_authority.key),
        &[
            program_data.clone(),
            current_authority.clone(),
            upgrade_authority.clone(),
            loader.clone(),
        ],
        &[&[seed, &[bump]]],
    )?;
    Ok(())
}

/// The accounts the loader's `Upgrade` takes, plus the loader itself.
pub struct UpgradeAccounts<'a, 'info> {
    pub program_data: &'a AccountInfo<'info>,
    pub program: &'a AccountInfo<'info>,
    pub buffer: &'a AccountInfo<'info>,
    pub spill: &'a AccountInfo<'info>,
    pub rent: &'a AccountInfo<'info>,
    pub clock: &'a AccountInfo<'info>,
    pub upgrade_authority: &'a AccountInfo<'info>,
    pub loader: &'a AccountInfo<'info>,
}

/// Replace `program_id`'s code with `buffer`'s, signed by its upgrade
/// authority PDA at `[seed]`; the buffer's rent goes to `spill`.
pub fn upgrade_program(
    program_id: &Pubkey,
    seed: &[u8],
    bump: u8,
    accounts: UpgradeAccounts,
) -> Result<()> {
    invoke_signed(
        &upgrade(
            program_id,
            accounts.buffer.key,
            accounts.upgrade_authority.key,
            accounts.spill.key,
        ),
        &[
            accounts.program_data.clone(),
            accounts.program.clone(),
            accounts.buffer.clone(),
            accounts.spill.clone(),
            accounts.rent.clone(),
            accounts.clock.clone(),
            accounts.upgrade_authority.clone(),
            accounts.loader.clone(),
        ],
        &[&[seed, &[bump]]],
    )?;
    Ok(())
}

/// The executable hash of the program a loader buffer holds: sha256 of its
/// code with trailing zero bytes removed, as `solana-verify
/// get-executable-hash` and `get-program-hash` compute it, so an upgrade's
/// announced hash names the verifiable build it installed. A buffer the
/// loader does not hold, or that holds no code, is refused.
pub fn executable_hash(buffer_data: &[u8]) -> Result<[u8; 32]> {
    let metadata = UpgradeableLoaderState::size_of_buffer_metadata();
    require!(
        matches!(
            bincode::deserialize(
                buffer_data
                    .get(..metadata)
                    .ok_or(PAError::InvalidUpgradeBuffer)?
            ),
            Ok(UpgradeableLoaderState::Buffer { .. })
        ),
        PAError::InvalidUpgradeBuffer
    );
    let code = &buffer_data[metadata..];
    let end = code
        .iter()
        .rposition(|&b| b != 0)
        .ok_or(PAError::InvalidUpgradeBuffer)?
        + 1;
    Ok(solana_sha256_hasher::hashv(&[&code[..end]]).to_bytes())
}
