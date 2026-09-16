//! SPL Token Forwarder - AnomaPay-style token wrap/unwrap for Solana PA.
//!
//! This forwarder implements the ERC20Forwarder pattern on Solana:
//! - Wrap: Lock tokens into escrow, authorized by Ed25519 signature
//! - Unwrap: Release tokens from escrow to recipient
//!
//! Security properties (mirroring EVM):
//! - Only Protocol Adapter can call forward_call
//! - Only handles specific logic_ref (resource type)
//! - User authorization via Ed25519 signature over action_tree_root

#![allow(deprecated)] // Anchor program macro currently expands to AccountInfo::realloc.

use anchor_lang::prelude::*;
use anchor_lang::solana_program::program::{invoke_signed, set_return_data};
use anchor_lang::solana_program::sysvar::instructions as ix_sysvar;

pub mod ed25519;
mod error;
pub mod state;
#[cfg(test)]
mod tests;

pub use error::ErrorCode;
pub use state::*;

declare_id!("5CrHbBeHjg53UyL3Htn9dCYYTy68fMcrbDoeAdo4yQrx");

/// Mirrors EVM: `event Wrapped(address indexed token, address indexed from, uint128 amount);`
#[event]
pub struct Wrapped {
    pub token_mint: Pubkey,
    pub from: Pubkey,
    pub amount: u64,
    pub nonce: u64,
    pub action_tree_root: [u8; 32],
}

/// Mirrors EVM: `event Unwrapped(address indexed token, address indexed to, uint128 amount);`
#[event]
pub struct Unwrapped {
    pub token_mint: Pubkey,
    pub to: Pubkey,
    pub amount: u64,
}

/// Mirrors EVM: `event EmergencyCallerSet(address caller);`
#[event]
pub struct EmergencyCallerSet {
    pub emergency_caller: Pubkey,
    pub set_by: Pubkey,
}

#[event]
pub struct EmergencyWithdraw {
    pub token_mint: Pubkey,
    pub to: Pubkey,
    pub amount: u64,
    pub caller: Pubkey,
}

/// Operation codes, the first byte of a `forward_call` input.
pub const OP_WRAP: u8 = 0;
pub const OP_UNWRAP: u8 = 1;

/// Return data of a successful `forward_call`.
pub const RESULT_SUCCESS: u8 = 1;

const SPL_TRANSFER_OPCODE: u8 = 3;
const SPL_CLOSE_ACCOUNT_OPCODE: u8 = 9;
const SPL_TOKEN_PROGRAM_ID: Pubkey =
    anchor_lang::solana_program::pubkey!("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA");
/// SPL token account layout: mint(32), owner(32), amount(8), ...
const TOKEN_ACCOUNT_OWNER_OFFSET: usize = 32;
const TOKEN_ACCOUNT_AMOUNT_OFFSET: usize = 64;

#[program]
pub mod spl_token_forwarder {
    use super::*;

    /// Initialize the forwarder config with emergency committee.
    ///
    /// Mirrors the EVM constructor validation: no zero adapter, logic ref, or committee.
    pub fn initialize(
        ctx: Context<Initialize>,
        protocol_adapter: Pubkey,
        logic_ref: [u8; 32],
        emergency_committee: Pubkey,
    ) -> Result<()> {
        require!(
            protocol_adapter != Pubkey::default(),
            ErrorCode::ZeroAddressNotAllowed
        );
        require!(logic_ref != [0u8; 32], ErrorCode::ZeroAddressNotAllowed);
        require!(
            emergency_committee != Pubkey::default(),
            ErrorCode::ZeroAddressNotAllowed
        );

        let config = &mut ctx.accounts.config;
        config.protocol_adapter = protocol_adapter;
        config.logic_ref = logic_ref;
        config.emergency_committee = emergency_committee;
        config.emergency_caller = Pubkey::default();
        config.bump = ctx.bumps.config;
        Ok(())
    }

    /// Create the nonce bitmap for one 256-nonce word of `user`, paid by
    /// whoever signs. Permissionless: the bitmap holds nothing but used
    /// bits, and a wrap requires it to exist. The adapter forwards no
    /// signer to a forwarder, so the account cannot be created during the
    /// wrap itself; a submitter sends this instruction ahead of settlement
    /// when the word's bitmap is missing.
    pub fn init_nonce_bitmap(
        _ctx: Context<InitNonceBitmap>,
        _user: Pubkey,
        _word_index: u64,
    ) -> Result<()> {
        Ok(())
    }

    /// Forward a wrap or unwrap call from the Protocol Adapter.
    ///
    /// Like EVM's ForwarderBase.forwardCall(), this does not check the
    /// adapter's stopped state: the adapter does not call forwarders once stopped.
    pub fn forward_call<'info>(
        ctx: Context<'_, '_, 'info, 'info, ForwardCall<'info>>,
        logic_ref: [u8; 32],
        input: Vec<u8>,
    ) -> Result<()> {
        let config = &ctx.accounts.config;

        // The instructions sysvar holds only top-level instructions, so
        // during a CPI the current instruction is the caller's. A direct
        // call sees this program's own id and is rejected. This is the
        // unforgeable analogue of EVM's msg.sender == _PROTOCOL_ADAPTER.
        let ix_sysvar_info = &ctx.accounts.ix_sysvar;
        let current_ix_index = ix_sysvar::load_current_index_checked(ix_sysvar_info)
            .map_err(|_| ErrorCode::UnauthorizedCaller)?;
        let current_ix =
            ix_sysvar::load_instruction_at_checked(current_ix_index as usize, ix_sysvar_info)
                .map_err(|_| ErrorCode::UnauthorizedCaller)?;
        require!(
            current_ix.program_id == config.protocol_adapter,
            ErrorCode::UnauthorizedCaller
        );
        require!(
            logic_ref == config.logic_ref,
            ErrorCode::UnauthorizedLogicRef
        );

        let (op, operand) = input.split_first().ok_or(ErrorCode::InvalidInput)?;
        match *op {
            OP_WRAP => execute_wrap(&ctx, operand)?,
            OP_UNWRAP => execute_unwrap(&ctx, operand)?,
            _ => return Err(ErrorCode::UnknownOperation.into()),
        }

        set_return_data(&[RESULT_SUCCESS]);
        Ok(())
    }

    /// Withdraw from escrow while the adapter is stopped, as the emergency
    /// caller. The operand is an `UnwrapInput`. Mirrors EVM's forwardEmergencyCall().
    pub fn forward_emergency_call(
        ctx: Context<ForwardEmergencyCall>,
        input: Vec<u8>,
    ) -> Result<()> {
        require_stopped_adapter(&ctx.accounts.config, &ctx.accounts.pa_state)?;
        let withdraw = UnwrapInput::try_from_bytes(&input)?;

        let [escrow_ata, recipient_ata, escrow_pda, token_program, ..] = ctx.remaining_accounts
        else {
            msg!(
                "Expected 4 remaining accounts for emergency withdraw, got {}",
                ctx.remaining_accounts.len()
            );
            return Err(ErrorCode::InsufficientRemainingAccounts.into());
        };

        require_token_account_owner(recipient_ata, &withdraw.recipient)?;
        transfer_signed_by_escrow(
            ctx.program_id,
            &withdraw.token_mint,
            escrow_ata,
            recipient_ata,
            escrow_pda,
            token_program,
            withdraw.amount,
        )?;

        emit!(EmergencyWithdraw {
            token_mint: withdraw.token_mint,
            to: withdraw.recipient,
            amount: withdraw.amount,
            caller: ctx.accounts.caller.key(),
        });
        Ok(())
    }

    /// Set the emergency caller, once, by the committee while the adapter
    /// is stopped. Mirrors EVM's setEmergencyCaller().
    pub fn set_emergency_caller(
        ctx: Context<SetEmergencyCaller>,
        new_emergency_caller: Pubkey,
    ) -> Result<()> {
        require_stopped_adapter(&ctx.accounts.config, &ctx.accounts.pa_state)?;
        require!(
            new_emergency_caller != Pubkey::default(),
            ErrorCode::ZeroAddressNotAllowed
        );
        let config = &mut ctx.accounts.config;
        require!(
            config.emergency_caller == Pubkey::default(),
            ErrorCode::EmergencyCallerAlreadySet
        );
        config.emergency_caller = new_emergency_caller;

        emit!(EmergencyCallerSet {
            emergency_caller: new_emergency_caller,
            set_by: ctx.accounts.committee.key(),
        });
        Ok(())
    }

    /// Drain a mint's escrow to the recipient and close the escrow token
    /// account; its rent goes to the committee.
    pub fn close_escrow(ctx: Context<CloseEscrow>) -> Result<()> {
        let token_mint_key = ctx.accounts.token_mint.key();
        let escrow_seeds: &[&[u8]] = &[
            ESCROW_SEED,
            token_mint_key.as_ref(),
            &[ctx.bumps.escrow_pda],
        ];
        let signer_seeds = &[escrow_seeds];

        let balance = token_account_amount(&ctx.accounts.escrow_ata)?;
        if balance > 0 {
            transfer_signed_by_escrow(
                ctx.program_id,
                &token_mint_key,
                &ctx.accounts.escrow_ata,
                &ctx.accounts.recipient_ata,
                &ctx.accounts.escrow_pda,
                &ctx.accounts.token_program,
                balance,
            )?;
        }

        let close_ix = spl_close_account_ix(
            ctx.accounts.escrow_ata.key,
            ctx.accounts.authority.key,
            ctx.accounts.escrow_pda.key,
        );
        invoke_signed(
            &close_ix,
            &[
                ctx.accounts.escrow_ata.to_account_info(),
                ctx.accounts.authority.to_account_info(),
                ctx.accounts.escrow_pda.to_account_info(),
            ],
            signer_seeds,
        )?;
        Ok(())
    }

    /// Close the config PDA; its rent goes to the committee. Call last.
    pub fn close_config(_ctx: Context<CloseConfig>) -> Result<()> {
        Ok(())
    }

    /// Close the nonce bitmaps passed as remaining accounts; their rent goes
    /// to the committee.
    pub fn close_nonce_bitmaps_batch<'info>(
        ctx: Context<'_, '_, 'info, 'info, CloseNonceBitmaps<'info>>,
    ) -> Result<()> {
        for bitmap in ctx.remaining_accounts {
            Account::<NonceBitmap>::try_from(bitmap)
                .map_err(|_| ErrorCode::InvalidNonceBitmapPda)?
                .close(ctx.accounts.authority.to_account_info())?;
        }
        Ok(())
    }
}

fn token_account_amount(token_account: &AccountInfo) -> Result<u64> {
    let data = token_account.try_borrow_data()?;
    let amount = data
        .get(TOKEN_ACCOUNT_AMOUNT_OFFSET..TOKEN_ACCOUNT_AMOUNT_OFFSET + 8)
        .ok_or(ErrorCode::InvalidTokenAccountData)?;
    Ok(u64::from_le_bytes(amount.try_into().unwrap()))
}

/// The destination of a forwarded transfer is chosen by the submitter, not
/// by the proof; it must belong to the party the proof-bound input names.
fn require_token_account_owner(token_account: &AccountInfo, owner: &Pubkey) -> Result<()> {
    let data = token_account.try_borrow_data()?;
    let actual = data
        .get(TOKEN_ACCOUNT_OWNER_OFFSET..TOKEN_ACCOUNT_OWNER_OFFSET + 32)
        .ok_or(ErrorCode::InvalidTokenAccountData)?;
    if actual != owner.as_ref() {
        msg!(
            "Token account {} is owned by {}, expected {}",
            token_account.key(),
            Pubkey::new_from_array(actual.try_into().unwrap()),
            owner
        );
        return Err(ErrorCode::WrongTokenAccountOwner.into());
    }
    Ok(())
}

/// Both emergency instructions require the adapter to be stopped. Mirrors
/// EVM's _checkEmergencyStopped(): the state account's address derives from
/// the configured adapter, and the lifecycle is read through the adapter's type.
fn require_stopped_adapter(config: &Config, pa_state: &AccountInfo) -> Result<()> {
    require!(
        pa_state.key() == derive_pa_state_pda(&config.protocol_adapter).0,
        ErrorCode::InvalidPaState
    );
    require!(
        pa_is_stopped(&pa_state.try_borrow_data()?)?,
        ErrorCode::ProtocolAdapterNotStopped
    );
    Ok(())
}

fn spl_transfer_ix(
    source: &Pubkey,
    destination: &Pubkey,
    authority: &Pubkey,
    amount: u64,
) -> anchor_lang::solana_program::instruction::Instruction {
    let mut data = [0u8; 9];
    data[0] = SPL_TRANSFER_OPCODE;
    data[1..9].copy_from_slice(&amount.to_le_bytes());

    anchor_lang::solana_program::instruction::Instruction {
        program_id: SPL_TOKEN_PROGRAM_ID,
        accounts: vec![
            AccountMeta::new(*source, false),
            AccountMeta::new(*destination, false),
            AccountMeta::new_readonly(*authority, true),
        ],
        data: data.to_vec(),
    }
}

fn spl_close_account_ix(
    account: &Pubkey,
    destination: &Pubkey,
    authority: &Pubkey,
) -> anchor_lang::solana_program::instruction::Instruction {
    anchor_lang::solana_program::instruction::Instruction {
        program_id: SPL_TOKEN_PROGRAM_ID,
        accounts: vec![
            AccountMeta::new(*account, false),
            AccountMeta::new(*destination, false),
            AccountMeta::new_readonly(*authority, true),
        ],
        data: vec![SPL_CLOSE_ACCOUNT_OPCODE],
    }
}

/// Transfer `amount` of `token_mint` from `source` to `destination` with the
/// mint's escrow PDA as the signing authority: as the user's delegate on a
/// wrap, as the escrow's owner on an unwrap or emergency withdraw. The SPL
/// Token program enforces the delegate approval and the balance; the
/// transferred amount is exact, so no before/after balance check is needed.
fn transfer_signed_by_escrow<'info>(
    program_id: &Pubkey,
    token_mint: &Pubkey,
    source: &AccountInfo<'info>,
    destination: &AccountInfo<'info>,
    escrow_pda: &AccountInfo<'info>,
    token_program: &AccountInfo<'info>,
    amount: u64,
) -> Result<()> {
    require!(
        token_program.key() == SPL_TOKEN_PROGRAM_ID,
        ErrorCode::InvalidTokenProgram
    );
    let (expected_escrow_pda, escrow_bump) = derive_escrow_pda(program_id, token_mint);
    require!(
        escrow_pda.key() == expected_escrow_pda,
        ErrorCode::InvalidEscrowPda
    );

    let transfer_ix = spl_transfer_ix(source.key, destination.key, escrow_pda.key, amount);
    let escrow_seeds = &[ESCROW_SEED, token_mint.as_ref(), &[escrow_bump]];
    invoke_signed(
        &transfer_ix,
        &[
            source.clone(),
            destination.clone(),
            escrow_pda.clone(),
            token_program.clone(),
        ],
        &[&escrow_seeds[..]],
    )
    .map_err(|e| {
        msg!("Token transfer failed: {:?}", e);
        ErrorCode::TokenTransferFailed
    })?;
    Ok(())
}

fn execute_wrap<'info>(
    ctx: &Context<'_, '_, 'info, 'info, ForwardCall<'info>>,
    input: &[u8],
) -> Result<()> {
    let wrap = WrapInput::try_from_bytes(input)?;

    // Permit2 / EIP-2612 semantics: allowed at the deadline, rejected after.
    let now = Clock::get()?.unix_timestamp;
    if now > wrap.deadline {
        msg!(
            "Deadline expired: current={}, deadline={}",
            now,
            wrap.deadline
        );
        return Err(ErrorCode::DeadlineExpired.into());
    }

    let [user_ata, escrow_ata, escrow_pda, nonce_bitmap_pda, token_program, ..] =
        ctx.remaining_accounts
    else {
        msg!(
            "Expected 5 remaining accounts for wrap, got {}",
            ctx.remaining_accounts.len()
        );
        return Err(ErrorCode::InsufficientRemainingAccounts.into());
    };

    // Reject replays before the signature check, which costs more.
    let (word_index, bit_position) = nonce_to_word_and_bit(wrap.nonce);
    let (expected_bitmap_pda, _) = derive_nonce_bitmap_pda(ctx.program_id, &wrap.user, word_index);
    require!(
        nonce_bitmap_pda.key() == expected_bitmap_pda,
        ErrorCode::InvalidNonceBitmapPda
    );
    // The bitmap must already exist (init_nonce_bitmap); the adapter's CPI
    // carries no signer that could pay for creating it here.
    let mut nonce_bitmap = Account::<NonceBitmap>::try_from(nonce_bitmap_pda).map_err(|_| {
        msg!(
            "Nonce bitmap {} for user {} word {} does not exist",
            nonce_bitmap_pda.key(),
            wrap.user,
            word_index
        );
        ErrorCode::NonceBitmapMissing
    })?;
    if nonce_bitmap.is_used(bit_position) {
        msg!("Nonce {} already used for user {}", wrap.nonce, wrap.user);
        return Err(ErrorCode::NonceAlreadyUsed.into());
    }

    // The proof binds the mint (through the escrow PDA) but not the
    // destination account; it must be one the escrow authority owns.
    require_token_account_owner(escrow_ata, escrow_pda.key)?;

    ed25519::verify_ed25519_instruction(
        &ctx.accounts.ix_sysvar,
        wrap.ed25519_ix_index,
        &wrap.user.to_bytes(),
        &wrap.to_message(ctx.program_id).signed_message(),
    )?;

    transfer_signed_by_escrow(
        ctx.program_id,
        &wrap.token_mint,
        user_ata,
        escrow_ata,
        escrow_pda,
        token_program,
        wrap.amount,
    )?;

    nonce_bitmap.mark_used(bit_position);
    nonce_bitmap.exit(ctx.program_id)?;

    emit!(Wrapped {
        token_mint: wrap.token_mint,
        from: wrap.user,
        amount: wrap.amount,
        nonce: wrap.nonce,
        action_tree_root: wrap.action_tree_root,
    });
    Ok(())
}

fn execute_unwrap(ctx: &Context<ForwardCall>, input: &[u8]) -> Result<()> {
    let unwrap = UnwrapInput::try_from_bytes(input)?;

    let [escrow_ata, recipient_ata, escrow_pda, token_program, ..] = ctx.remaining_accounts else {
        msg!(
            "Expected 4 remaining accounts for unwrap, got {}",
            ctx.remaining_accounts.len()
        );
        return Err(ErrorCode::InsufficientRemainingAccounts.into());
    };

    require_token_account_owner(recipient_ata, &unwrap.recipient)?;
    transfer_signed_by_escrow(
        ctx.program_id,
        &unwrap.token_mint,
        escrow_ata,
        recipient_ata,
        escrow_pda,
        token_program,
        unwrap.amount,
    )?;

    emit!(Unwrapped {
        token_mint: unwrap.token_mint,
        to: unwrap.recipient,
        amount: unwrap.amount,
    });
    Ok(())
}

#[derive(Accounts)]
pub struct Initialize<'info> {
    #[account(mut)]
    pub authority: Signer<'info>,

    #[account(
        init,
        payer = authority,
        space = 8 + Config::INIT_SPACE,
        seeds = [CONFIG_SEED],
        bump
    )]
    pub config: Account<'info, Config>,

    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
#[instruction(user: Pubkey, word_index: u64)]
pub struct InitNonceBitmap<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,

    #[account(
        init,
        payer = payer,
        space = NonceBitmap::ACCOUNT_SIZE,
        seeds = [NONCE_BITMAP_SEED, user.as_ref(), &word_index.to_le_bytes()],
        bump
    )]
    pub nonce_bitmap: Account<'info, NonceBitmap>,

    pub system_program: Program<'info, System>,
}

/// The adapter's CPI segment: the config, the instructions sysvar (caller
/// verification and ed25519 introspection), then the operation's accounts
/// as remaining accounts.
#[derive(Accounts)]
pub struct ForwardCall<'info> {
    #[account(seeds = [CONFIG_SEED], bump = config.bump)]
    pub config: Account<'info, Config>,

    /// CHECK: Validated via address constraint
    #[account(address = anchor_lang::solana_program::sysvar::instructions::ID)]
    pub ix_sysvar: AccountInfo<'info>,
}

#[derive(Accounts)]
pub struct ForwardEmergencyCall<'info> {
    #[account(mut)]
    pub caller: Signer<'info>,

    #[account(
        seeds = [CONFIG_SEED],
        bump = config.bump,
        constraint = config.emergency_caller != Pubkey::default() @ ErrorCode::EmergencyCallerNotSet,
        constraint = config.emergency_caller == caller.key() @ ErrorCode::UnauthorizedCaller,
    )]
    pub config: Account<'info, Config>,

    /// CHECK: Checked by require_stopped_adapter in the handler.
    pub pa_state: AccountInfo<'info>,
}

#[derive(Accounts)]
pub struct SetEmergencyCaller<'info> {
    pub committee: Signer<'info>,

    #[account(
        mut,
        seeds = [CONFIG_SEED],
        bump = config.bump,
        constraint = config.emergency_committee == committee.key() @ ErrorCode::UnauthorizedCaller
    )]
    pub config: Account<'info, Config>,

    /// CHECK: Checked by require_stopped_adapter in the handler.
    pub pa_state: AccountInfo<'info>,
}

#[derive(Accounts)]
pub struct CloseEscrow<'info> {
    /// Emergency committee, receives the closed escrow ATA's rent.
    #[account(
        mut,
        constraint = config.emergency_committee == authority.key() @ ErrorCode::UnauthorizedCaller
    )]
    pub authority: Signer<'info>,

    #[account(seeds = [CONFIG_SEED], bump = config.bump)]
    pub config: Account<'info, Config>,

    /// CHECK: Ownership verified by the SPL Token program during the transfer and close CPIs.
    #[account(mut)]
    pub escrow_ata: AccountInfo<'info>,

    /// CHECK: Derived from seeds; the SPL Token CPI verifies it owns the escrow ATA.
    #[account(seeds = [ESCROW_SEED, token_mint.key().as_ref()], bump)]
    pub escrow_pda: AccountInfo<'info>,

    /// CHECK: Passed to the SPL Token transfer CPI.
    #[account(mut)]
    pub recipient_ata: AccountInfo<'info>,

    /// CHECK: Seed of escrow_pda; the seeds constraint ties it to the escrow.
    pub token_mint: AccountInfo<'info>,

    /// CHECK: Verified by address constraint.
    #[account(address = SPL_TOKEN_PROGRAM_ID)]
    pub token_program: AccountInfo<'info>,
}

#[derive(Accounts)]
pub struct CloseConfig<'info> {
    /// Emergency committee, receives the config's rent.
    #[account(mut)]
    pub authority: Signer<'info>,

    #[account(
        mut,
        seeds = [CONFIG_SEED],
        bump = config.bump,
        constraint = config.emergency_committee == authority.key() @ ErrorCode::UnauthorizedCaller,
        close = authority
    )]
    pub config: Account<'info, Config>,
}

#[derive(Accounts)]
pub struct CloseNonceBitmaps<'info> {
    /// Emergency committee, receives the closed bitmaps' rent.
    #[account(
        mut,
        constraint = config.emergency_committee == authority.key() @ ErrorCode::UnauthorizedCaller
    )]
    pub authority: Signer<'info>,

    #[account(seeds = [CONFIG_SEED], bump = config.bump)]
    pub config: Account<'info, Config>,
}
