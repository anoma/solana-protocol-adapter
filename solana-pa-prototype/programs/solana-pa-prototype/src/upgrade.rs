//! A program that owns its own upgrades, as a UUPS implementation does: the
//! loader's upgrade authority is a PDA of the program, so the loader accepts
//! an upgrade (or an authority change) only when the program signs for it,
//! which it does only for its owner. Shared by the adapter and the SPL token
//! forwarder, whose owner-only `upgrade` instructions wrap `upgrade_program`.

use anchor_lang::prelude::*;
use anchor_lang::solana_program::bpf_loader_upgradeable::{
    set_buffer_authority_checked, set_upgrade_authority_checked, upgrade, UpgradeableLoaderState,
};
use anchor_lang::solana_program::program::invoke_signed;

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

/// The accounts the loader's `Upgrade` takes, the buffer's current authority
/// (the owner, a signer), and the loader itself.
pub struct UpgradeAccounts<'a, 'info> {
    pub program_data: &'a AccountInfo<'info>,
    pub program: &'a AccountInfo<'info>,
    pub buffer: &'a AccountInfo<'info>,
    pub buffer_authority: &'a AccountInfo<'info>,
    pub spill: &'a AccountInfo<'info>,
    pub rent: &'a AccountInfo<'info>,
    pub clock: &'a AccountInfo<'info>,
    pub upgrade_authority: &'a AccountInfo<'info>,
    pub loader: &'a AccountInfo<'info>,
}

/// Replace `program_id`'s code with `buffer`'s: hand the buffer from its
/// authority (a signer) to the program's upgrade authority PDA at `[seed]`,
/// as the loader requires the buffer's and the program's authorities to
/// match, then upgrade, both signed by the PDA. The buffer's rent goes to
/// `spill`.
pub fn upgrade_program(
    program_id: &Pubkey,
    seed: &[u8],
    bump: u8,
    accounts: UpgradeAccounts,
) -> Result<()> {
    invoke_signed(
        &set_buffer_authority_checked(
            accounts.buffer.key,
            accounts.buffer_authority.key,
            accounts.upgrade_authority.key,
        ),
        &[
            accounts.buffer.clone(),
            accounts.buffer_authority.clone(),
            accounts.upgrade_authority.clone(),
            accounts.loader.clone(),
        ],
        &[&[seed, &[bump]]],
    )?;
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
/// announced hash names the verifiable build it installed. None for data
/// that is not a loader buffer, or a buffer that holds no code; each program
/// refuses that with its own error.
pub fn executable_hash(buffer_data: &[u8]) -> Option<[u8; 32]> {
    let metadata = UpgradeableLoaderState::size_of_buffer_metadata();
    let state = bincode::deserialize(buffer_data.get(..metadata)?).ok()?;
    let UpgradeableLoaderState::Buffer { .. } = state else {
        return None;
    };
    let code = &buffer_data[metadata..];
    let end = code.iter().rposition(|&b| b != 0)? + 1;
    Some(solana_sha256_hasher::hashv(&[&code[..end]]).to_bytes())
}
