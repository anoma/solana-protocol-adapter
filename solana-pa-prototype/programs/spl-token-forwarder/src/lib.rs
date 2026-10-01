//! SPL Token Forwarder - AnomaPay-style token wrap/unwrap for Solana PA.
//!
//! This forwarder implements the ERC20Forwarder pattern on Solana:
//! - Wrap: Lock tokens into escrow, authorized by Ed25519 signature
//! - Unwrap: Release tokens from escrow to recipient
//!
//! Security properties (mirroring EVM):
//! - Only the Protocol Adapter's own instruction can call forward_call (directly, not through another program)
//! - Only handles specific logic_ref (resource type)
//! - User authorization via Ed25519 signature over action_tree_root

use anchor_lang::prelude::*;
use anchor_lang::solana_program::instruction::{get_stack_height, TRANSACTION_LEVEL_STACK_HEIGHT};
use anchor_lang::solana_program::program::{invoke_signed, set_return_data};
use solana_instructions_sysvar as ix_sysvar;

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

/// The EVM V1 forwarder's `event EmergencyCallerSet(address caller);` (V1 emergency mechanism, anoma/dos-pm#86).
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

/// Mirrors OpenZeppelin Initializable: `event Initialized(uint64 version);`
#[event]
pub struct Initialized {
    pub version: u64,
}

/// Operation codes, the first byte of a `forward_call` input.
pub const OP_WRAP: u8 = 0;
#[constant]
pub const OP_UNWRAP: u8 = 1;

/// Return data of a successful `forward_call`.
pub const RESULT_SUCCESS: u8 = 1;

const SPL_TRANSFER_OPCODE: u8 = 3;
const SPL_CLOSE_ACCOUNT_OPCODE: u8 = 9;
const SPL_TOKEN_PROGRAM_ID: Pubkey = pubkey!("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA");
/// SPL token account layout: mint(32), owner(32), amount(8), ...
const TOKEN_ACCOUNT_MINT_OFFSET: usize = 0;
const TOKEN_ACCOUNT_OWNER_OFFSET: usize = 32;
const TOKEN_ACCOUNT_AMOUNT_OFFSET: usize = 64;

#[program]
pub mod spl_token_forwarder {
    use super::*;

    /// Initialize the forwarder config with emergency committee.
    ///
    /// As the EVM V2 forwarder's initializer: no zero adapter or logic ref.
    /// The committee, which has no V2 counterpart, must also be nonzero.
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
        config.version = CONFIG_VERSION;
        emit!(Initialized {
            version: CONFIG_VERSION
        });
        Ok(())
    }

    /// Rotate the logic ref, once per build that raises CONFIG_VERSION.
    /// Mirrors the EVM forwarder's rotation: the owner upgrades the proxy to
    /// an implementation whose `reinitializer(n)` writes the new ref
    /// (`upgradeToAndCall`), and escrow and nonces stay. Here
    /// the upgrade authority upgrades the program in place and then calls
    /// this; it runs only while the config's version is below this build's.
    /// Resources under the previous ref leave through the new one once the
    /// kind table lists the previous version as its alias: a transaction
    /// converts them, and the new resource unwraps.
    pub fn reinitialize(ctx: Context<Reinitialize>, logic_ref: [u8; 32]) -> Result<()> {
        let config = &mut ctx.accounts.config;
        require!(
            config.version < CONFIG_VERSION,
            ErrorCode::InvalidInitialization
        );
        require!(logic_ref != [0u8; 32], ErrorCode::ZeroAddressNotAllowed);
        config.logic_ref = logic_ref;
        config.version = CONFIG_VERSION;
        emit!(Initialized {
            version: CONFIG_VERSION
        });
        Ok(())
    }

    /// The release this build is. Mirrors the EVM forwarder's `VERSION`:
    /// read from the deployed program (by simulation), it names the code an
    /// address runs, which an in-place upgrade changes.
    pub fn version(_ctx: Context<Version>) -> Result<String> {
        Ok(env!("CARGO_PKG_VERSION").to_string())
    }

    /// Bring the config the previous build created to this build's layout:
    /// the bump it stored after the fields gives way to the version, 1, the
    /// previous build having initialized it once and never reinitialized it;
    /// the owner pays the added bytes' rent. The migrations are the
    /// counterpart of the call the EVM owner passes to `upgradeToAndCall`:
    /// the upgrade authority runs them once, after upgrading the program in
    /// place.
    pub fn migrate_config(ctx: Context<MigrateConfig>) -> Result<()> {
        let config = &ctx.accounts.config;
        require!(
            config.owner == ctx.program_id
                && config.data_len() as u64 == PREVIOUS_CONFIG_SIZE
                && config.try_borrow_data()?.starts_with(Config::DISCRIMINATOR),
            ErrorCode::NotPreviousLayout
        );
        grow(
            config,
            &ctx.accounts.authority,
            &ctx.accounts.system_program,
            Config::ACCOUNT_SIZE,
        )?;
        let mut data = config.try_borrow_mut_data()?;
        let mut migrated = Config::try_deserialize_unchecked(&mut &data[..])?;
        migrated.version = 1;
        migrated.try_serialize(&mut &mut data[..])?;
        Ok(())
    }

    /// Bring a nonce bitmap the previous build created to this build's
    /// layout: its used bits are kept and its canonical bump is appended,
    /// the owner paying the added byte's rent.
    pub fn migrate_nonce_bitmap(
        ctx: Context<MigrateNonceBitmap>,
        _user: Pubkey,
        _word_index: u64,
    ) -> Result<()> {
        let bitmap = &ctx.accounts.nonce_bitmap;
        require!(
            bitmap.owner == ctx.program_id
                && bitmap.data_len() as u64 == PREVIOUS_NONCE_BITMAP_SIZE
                && bitmap
                    .try_borrow_data()?
                    .starts_with(NonceBitmap::DISCRIMINATOR),
            ErrorCode::NotPreviousLayout
        );
        grow(
            bitmap,
            &ctx.accounts.authority,
            &ctx.accounts.system_program,
            NonceBitmap::ACCOUNT_SIZE,
        )?;
        bitmap.try_borrow_mut_data()?[NonceBitmap::ACCOUNT_SIZE - 1] = ctx.bumps.nonce_bitmap;
        Ok(())
    }

    /// Move a mint's escrowed tokens from the previous build's escrow, held
    /// by that mint's own authority `["escrow", mint]`, to the escrow of the
    /// one authority this build holds every mint under, and close the
    /// previous escrow account; its rent goes to the owner.
    pub fn migrate_escrow(ctx: Context<MigrateEscrow>) -> Result<()> {
        let mint = ctx.accounts.token_mint.key();
        let previous_ata = &ctx.accounts.previous_escrow_ata;
        let previous_authority = &ctx.accounts.previous_escrow_authority;
        require_token_account(previous_ata, &mint, previous_authority.key)?;
        require_token_account(&ctx.accounts.escrow_ata, &mint, &ESCROW_AUTHORITY)?;
        let previous_signer_seeds: &[&[u8]] = &[
            ESCROW_SEED,
            mint.as_ref(),
            &[ctx.bumps.previous_escrow_authority],
        ];

        let balance = token_account_amount(previous_ata)?;
        if balance > 0 {
            invoke_signed(
                &spl_transfer_ix(
                    previous_ata.key,
                    ctx.accounts.escrow_ata.key,
                    previous_authority.key,
                    balance,
                ),
                &[
                    previous_ata.to_account_info(),
                    ctx.accounts.escrow_ata.to_account_info(),
                    previous_authority.to_account_info(),
                ],
                &[previous_signer_seeds],
            )?;
        }
        invoke_signed(
            &spl_close_account_ix(
                previous_ata.key,
                ctx.accounts.authority.key,
                previous_authority.key,
            ),
            &[
                previous_ata.to_account_info(),
                ctx.accounts.authority.to_account_info(),
                previous_authority.to_account_info(),
            ],
            &[previous_signer_seeds],
        )?;
        Ok(())
    }

    /// Create the nonce bitmap for one 256-nonce word of `user`, paid by
    /// whoever signs. Permissionless: the bitmap holds nothing but used
    /// bits, and a wrap requires it to exist. The adapter forwards no
    /// signer to a forwarder, so the account cannot be created during the
    /// wrap itself; a submitter sends this instruction ahead of settlement
    /// when the word's bitmap is missing.
    pub fn init_nonce_bitmap(
        ctx: Context<InitNonceBitmap>,
        _user: Pubkey,
        _word_index: u64,
    ) -> Result<()> {
        ctx.accounts.nonce_bitmap.bump = ctx.bumps.nonce_bitmap;
        Ok(())
    }

    /// Forward a wrap or unwrap call from the Protocol Adapter.
    ///
    /// Like the EVM V2 `ForwarderBaseUpgradeable.forwardCall()`, this does not check the
    /// adapter's paused state: the adapter does not call forwarders while paused.
    pub fn forward_call<'info>(
        ctx: Context<'info, ForwardCall<'info>>,
        logic_ref: [u8; 32],
        input: Vec<u8>,
    ) -> Result<()> {
        let config = &ctx.accounts.config;

        // The instructions sysvar holds only top-level instructions, so it
        // names the program of the transaction-level instruction this call
        // descends from. That program is the immediate caller only when this
        // call runs one level below it; deeper, another program invoked us.
        // Together the two checks are the analogue of EVM's
        // msg.sender == $._protocolAdapter.
        require!(
            get_stack_height() == TRANSACTION_LEVEL_STACK_HEIGHT + 1,
            ErrorCode::UnauthorizedCaller
        );
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

    /// Withdraw from escrow while the adapter is paused, as the emergency
    /// caller. The operand is an `UnwrapInput`. The EVM V1 forwarder's
    /// forwardEmergencyCall(); V2 has no emergency path (anoma/dos-pm#86).
    pub fn forward_emergency_call(
        ctx: Context<ForwardEmergencyCall>,
        input: Vec<u8>,
    ) -> Result<()> {
        require_paused_adapter(&ctx.accounts.config, &ctx.accounts.pa_state)?;
        let withdraw = UnwrapInput::try_from_bytes(&input)?;

        let [escrow_ata, recipient_ata, escrow_authority, token_program, ..] =
            ctx.remaining_accounts
        else {
            msg!(
                "Expected 4 remaining accounts for emergency withdraw, got {}",
                ctx.remaining_accounts.len()
            );
            return Err(ErrorCode::InsufficientRemainingAccounts.into());
        };

        require_token_account(escrow_ata, &withdraw.token_mint, escrow_authority.key)?;
        require_token_account(recipient_ata, &withdraw.token_mint, &withdraw.recipient)?;
        transfer_signed_by_escrow(
            escrow_ata,
            recipient_ata,
            escrow_authority,
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
    /// is paused. The EVM V1 forwarder's setEmergencyCaller(); V2 has no
    /// emergency path (anoma/dos-pm#86).
    pub fn set_emergency_caller(
        ctx: Context<SetEmergencyCaller>,
        new_emergency_caller: Pubkey,
    ) -> Result<()> {
        require_paused_adapter(&ctx.accounts.config, &ctx.accounts.pa_state)?;
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
    /// account; its rent goes to the committee. Requires the adapter to be
    /// paused.
    pub fn close_escrow(ctx: Context<CloseEscrow>) -> Result<()> {
        require_paused_adapter(&ctx.accounts.config, &ctx.accounts.pa_state)?;
        let token_mint_key = ctx.accounts.token_mint.key();
        require_token_account(
            &ctx.accounts.escrow_ata,
            &token_mint_key,
            ctx.accounts.escrow_authority.key,
        )?;
        let balance = token_account_amount(&ctx.accounts.escrow_ata)?;
        if balance > 0 {
            transfer_signed_by_escrow(
                &ctx.accounts.escrow_ata,
                &ctx.accounts.recipient_ata,
                &ctx.accounts.escrow_authority,
                &ctx.accounts.token_program,
                balance,
            )?;
        }

        let close_ix = spl_close_account_ix(
            ctx.accounts.escrow_ata.key,
            ctx.accounts.authority.key,
            ctx.accounts.escrow_authority.key,
        );
        invoke_signed(
            &close_ix,
            &[
                ctx.accounts.escrow_ata.to_account_info(),
                ctx.accounts.authority.to_account_info(),
                ctx.accounts.escrow_authority.to_account_info(),
            ],
            &[ESCROW_SIGNER_SEEDS],
        )?;
        Ok(())
    }

    /// Close the config PDA; its rent goes to the committee. Call last.
    /// Requires the adapter to be paused.
    pub fn close_config(ctx: Context<CloseConfig>) -> Result<()> {
        require_paused_adapter(&ctx.accounts.config, &ctx.accounts.pa_state)?;
        Ok(())
    }

    /// Close the nonce bitmaps passed as remaining accounts; their rent goes
    /// to the committee. Requires the adapter to be paused.
    pub fn close_nonce_bitmaps_batch<'info>(
        ctx: Context<'info, CloseNonceBitmaps<'info>>,
    ) -> Result<()> {
        require_paused_adapter(&ctx.accounts.config, &ctx.accounts.pa_state)?;
        for bitmap in ctx.remaining_accounts {
            Account::<NonceBitmap>::try_from(bitmap)
                .map_err(|_| ErrorCode::InvalidNonceBitmapPda)?
                .close(ctx.accounts.authority.to_account_info())?;
        }
        Ok(())
    }
}

/// Grow a migrated account to `size`, the upgrade authority paying any rent
/// shortfall.
fn grow<'info>(
    account: &AccountInfo<'info>,
    payer: &Signer<'info>,
    system_program: &Program<'info, System>,
    size: usize,
) -> Result<()> {
    let shortfall = Rent::get()?
        .minimum_balance(size)
        .saturating_sub(account.lamports());
    if shortfall > 0 {
        anchor_lang::system_program::transfer(
            CpiContext::new(
                system_program.key(),
                anchor_lang::system_program::Transfer {
                    from: payer.to_account_info(),
                    to: account.clone(),
                },
            ),
            shortfall,
        )?;
    }
    account.resize(size)?;
    Ok(())
}

fn token_account_amount(token_account: &AccountInfo) -> Result<u64> {
    let data = token_account.try_borrow_data()?;
    let amount = data
        .get(TOKEN_ACCOUNT_AMOUNT_OFFSET..TOKEN_ACCOUNT_AMOUNT_OFFSET + 8)
        .ok_or(ErrorCode::InvalidTokenAccountData)?;
    Ok(u64::from_le_bytes(amount.try_into().unwrap()))
}

/// Token accounts of a forwarded transfer are chosen by the submitter, not
/// by the proof, and SPL Transfer checks only that source and destination
/// share a mint. Each must hold the mint the input names and belong to the
/// party the transfer names: the escrow on its side, the user or the
/// recipient on the other.
fn require_token_account(token_account: &AccountInfo, mint: &Pubkey, owner: &Pubkey) -> Result<()> {
    let data = token_account.try_borrow_data()?;
    let field = |offset: usize| -> Result<Pubkey> {
        let bytes: [u8; 32] = data
            .get(offset..offset + 32)
            .ok_or(ErrorCode::InvalidTokenAccountData)?
            .try_into()
            .unwrap();
        Ok(Pubkey::new_from_array(bytes))
    };
    let actual_mint = field(TOKEN_ACCOUNT_MINT_OFFSET)?;
    if actual_mint != *mint {
        msg!(
            "Token account {} holds mint {}, expected {}",
            token_account.key(),
            actual_mint,
            mint
        );
        return Err(ErrorCode::WrongTokenAccountMint.into());
    }
    let actual_owner = field(TOKEN_ACCOUNT_OWNER_OFFSET)?;
    if actual_owner != *owner {
        msg!(
            "Token account {} is owned by {}, expected {}",
            token_account.key(),
            actual_owner,
            owner
        );
        return Err(ErrorCode::WrongTokenAccountOwner.into());
    }
    Ok(())
}

/// Every committee and emergency-caller instruction requires the adapter to
/// be paused (the EVM V1 forwarder's _checkEmergencyStopped() required a
/// stopped adapter): the state account's
/// address derives from the configured adapter, and the paused flag is read
/// through the adapter's type.
fn require_paused_adapter(config: &Config, pa_state: &AccountInfo) -> Result<()> {
    require!(
        pa_state.key() == derive_pa_state_pda(&config.protocol_adapter).0,
        ErrorCode::InvalidPaState
    );
    require!(
        pa_is_paused(&pa_state.try_borrow_data()?)?,
        ErrorCode::ProtocolAdapterNotPaused
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

/// Transfer `amount` from `source` to `destination` with the escrow
/// authority as the signer: as the user's delegate on a wrap, as the escrow
/// account's owner on an unwrap or emergency withdraw. The SPL Token program
/// enforces the delegate approval, the balance and that both accounts hold
/// the same mint; the transferred amount is exact, so no before/after
/// balance check is needed.
fn transfer_signed_by_escrow<'info>(
    source: &AccountInfo<'info>,
    destination: &AccountInfo<'info>,
    escrow_authority: &AccountInfo<'info>,
    token_program: &AccountInfo<'info>,
    amount: u64,
) -> Result<()> {
    require!(
        token_program.key() == SPL_TOKEN_PROGRAM_ID,
        ErrorCode::InvalidTokenProgram
    );
    require!(
        escrow_authority.key() == ESCROW_AUTHORITY,
        ErrorCode::InvalidEscrowAuthority
    );

    let transfer_ix = spl_transfer_ix(source.key, destination.key, escrow_authority.key, amount);
    invoke_signed(
        &transfer_ix,
        &[
            source.clone(),
            destination.clone(),
            escrow_authority.clone(),
            token_program.clone(),
        ],
        &[ESCROW_SIGNER_SEEDS],
    )
    .map_err(|e| {
        msg!("Token transfer failed: {:?}", e);
        ErrorCode::TokenTransferFailed
    })?;
    Ok(())
}

fn execute_wrap<'info>(ctx: &Context<'info, ForwardCall<'info>>, input: &[u8]) -> Result<()> {
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

    let [user_ata, escrow_ata, escrow_authority, nonce_bitmap_pda, token_program, ..] =
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
    require!(
        nonce_bitmap.is_at(nonce_bitmap_pda.key, ctx.program_id, &wrap.user, word_index),
        ErrorCode::InvalidNonceBitmapPda
    );
    if nonce_bitmap.is_used(bit_position) {
        msg!("Nonce {} already used for user {}", wrap.nonce, wrap.user);
        return Err(ErrorCode::NonceAlreadyUsed.into());
    }

    // Both accounts are bound to the input's mint: the source to the signing
    // user, as Permit2 transfers from the signing owner; the destination to
    // the escrow.
    require_token_account(user_ata, &wrap.token_mint, &wrap.user)?;
    require_token_account(escrow_ata, &wrap.token_mint, escrow_authority.key)?;

    ed25519::verify_ed25519_instruction(
        &ctx.accounts.ix_sysvar,
        wrap.ed25519_ix_index,
        &wrap.user.to_bytes(),
        &wrap.to_message(ctx.program_id).signed_message(),
    )?;

    transfer_signed_by_escrow(
        user_ata,
        escrow_ata,
        escrow_authority,
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
    // Released to the escrow authority, the tokens would never leave custody
    // while the resource is spent; the EVM forwarder reverts an unwrap to
    // itself (its balance does not grow by the amount).
    require!(
        unwrap.recipient != ESCROW_AUTHORITY,
        ErrorCode::UnwrapToEscrow
    );

    let [escrow_ata, recipient_ata, escrow_authority, token_program, ..] = ctx.remaining_accounts
    else {
        msg!(
            "Expected 4 remaining accounts for unwrap, got {}",
            ctx.remaining_accounts.len()
        );
        return Err(ErrorCode::InsufficientRemainingAccounts.into());
    };

    require_token_account(escrow_ata, &unwrap.token_mint, escrow_authority.key)?;
    require_token_account(recipient_ata, &unwrap.token_mint, &unwrap.recipient)?;
    transfer_signed_by_escrow(
        escrow_ata,
        recipient_ata,
        escrow_authority,
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

/// The upgrade authority initializes the config, as the EVM proxy runs its
/// initializer atomically at deployment: whoever initializes names the
/// adapter the forwarder obeys and the committee.
#[derive(Accounts)]
pub struct Initialize<'info> {
    #[account(mut)]
    pub authority: Signer<'info>,

    #[account(
        init,
        payer = authority,
        space = Config::ACCOUNT_SIZE,
        seeds = [CONFIG_SEED],
        bump
    )]
    pub config: Account<'info, Config>,

    /// The program account proves `program_data` is this program's own
    /// ProgramData address rather than any account shaped like one.
    #[account(constraint = program.programdata_address()? == Some(program_data.key()) @ ErrorCode::UnauthorizedCaller)]
    pub program: Program<'info, crate::program::SplTokenForwarder>,

    /// The loader records the upgrade authority here; it is the forwarder's owner.
    #[account(constraint = program_data.upgrade_authority_address == Some(authority.key()) @ ErrorCode::UnauthorizedCaller)]
    pub program_data: Account<'info, ProgramData>,

    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct Version {}

#[derive(Accounts)]
pub struct Reinitialize<'info> {
    pub authority: Signer<'info>,

    #[account(mut, address = CONFIG_PDA)]
    pub config: Account<'info, Config>,

    /// The program account proves `program_data` is this program's own
    /// ProgramData address rather than any account shaped like one.
    #[account(constraint = program.programdata_address()? == Some(program_data.key()) @ ErrorCode::UnauthorizedCaller)]
    pub program: Program<'info, crate::program::SplTokenForwarder>,

    /// The loader records the upgrade authority here; it is the forwarder's owner.
    #[account(constraint = program_data.upgrade_authority_address == Some(authority.key()) @ ErrorCode::UnauthorizedCaller)]
    pub program_data: Account<'info, ProgramData>,
}

#[derive(Accounts)]
pub struct MigrateConfig<'info> {
    #[account(mut)]
    pub authority: Signer<'info>,

    /// CHECK: Pinned to the config address; the handler requires the
    /// previous build's config layout, owned by this program.
    #[account(mut, address = CONFIG_PDA)]
    pub config: UncheckedAccount<'info>,

    /// The program account proves `program_data` is this program's own
    /// ProgramData address rather than any account shaped like one.
    #[account(constraint = program.programdata_address()? == Some(program_data.key()) @ ErrorCode::UnauthorizedCaller)]
    pub program: Program<'info, crate::program::SplTokenForwarder>,

    /// The loader records the upgrade authority here; it is the forwarder's owner.
    #[account(constraint = program_data.upgrade_authority_address == Some(authority.key()) @ ErrorCode::UnauthorizedCaller)]
    pub program_data: Account<'info, ProgramData>,

    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
#[instruction(user: Pubkey, word_index: u64)]
pub struct MigrateNonceBitmap<'info> {
    #[account(mut)]
    pub authority: Signer<'info>,

    /// CHECK: The seeds pin it to `user`'s bitmap for `word_index`; the
    /// handler requires the previous build's bitmap layout, owned by this program.
    #[account(
        mut,
        seeds = [NONCE_BITMAP_SEED, user.as_ref(), &word_index.to_le_bytes()],
        bump
    )]
    pub nonce_bitmap: UncheckedAccount<'info>,

    /// The program account proves `program_data` is this program's own
    /// ProgramData address rather than any account shaped like one.
    #[account(constraint = program.programdata_address()? == Some(program_data.key()) @ ErrorCode::UnauthorizedCaller)]
    pub program: Program<'info, crate::program::SplTokenForwarder>,

    /// The loader records the upgrade authority here; it is the forwarder's owner.
    #[account(constraint = program_data.upgrade_authority_address == Some(authority.key()) @ ErrorCode::UnauthorizedCaller)]
    pub program_data: Account<'info, ProgramData>,

    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct MigrateEscrow<'info> {
    /// Receives the closed previous escrow account's rent.
    #[account(mut)]
    pub authority: Signer<'info>,

    /// CHECK: The mint whose escrow moves; the handler requires both escrow
    /// accounts to hold it.
    pub token_mint: UncheckedAccount<'info>,

    /// CHECK: The previous build's escrow authority for this mint, pinned by
    /// its seeds; it signs the move and the close.
    #[account(seeds = [ESCROW_SEED, token_mint.key().as_ref()], bump)]
    pub previous_escrow_authority: UncheckedAccount<'info>,

    /// CHECK: The handler requires it to hold the mint and belong to the previous escrow authority.
    #[account(mut)]
    pub previous_escrow_ata: UncheckedAccount<'info>,

    /// CHECK: The handler requires it to hold the mint and belong to the escrow authority.
    #[account(mut)]
    pub escrow_ata: UncheckedAccount<'info>,

    /// CHECK: Verified by address constraint.
    #[account(address = SPL_TOKEN_PROGRAM_ID)]
    pub token_program: UncheckedAccount<'info>,

    /// The program account proves `program_data` is this program's own
    /// ProgramData address rather than any account shaped like one.
    #[account(constraint = program.programdata_address()? == Some(program_data.key()) @ ErrorCode::UnauthorizedCaller)]
    pub program: Program<'info, crate::program::SplTokenForwarder>,

    /// The loader records the upgrade authority here; it is the forwarder's owner.
    #[account(constraint = program_data.upgrade_authority_address == Some(authority.key()) @ ErrorCode::UnauthorizedCaller)]
    pub program_data: Account<'info, ProgramData>,
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
    #[account(address = CONFIG_PDA)]
    pub config: Account<'info, Config>,

    /// CHECK: Validated via address constraint
    #[account(address = solana_instructions_sysvar::ID)]
    pub ix_sysvar: UncheckedAccount<'info>,
}

#[derive(Accounts)]
pub struct ForwardEmergencyCall<'info> {
    #[account(mut)]
    pub caller: Signer<'info>,

    #[account(
        address = CONFIG_PDA,
        constraint = config.emergency_caller != Pubkey::default() @ ErrorCode::EmergencyCallerNotSet,
        constraint = config.emergency_caller == caller.key() @ ErrorCode::UnauthorizedCaller,
    )]
    pub config: Account<'info, Config>,

    /// CHECK: Checked by require_paused_adapter in the handler.
    pub pa_state: UncheckedAccount<'info>,
}

#[derive(Accounts)]
pub struct SetEmergencyCaller<'info> {
    pub committee: Signer<'info>,

    #[account(
        mut,
        address = CONFIG_PDA,
        constraint = config.emergency_committee == committee.key() @ ErrorCode::UnauthorizedCaller
    )]
    pub config: Account<'info, Config>,

    /// CHECK: Checked by require_paused_adapter in the handler.
    pub pa_state: UncheckedAccount<'info>,
}

#[derive(Accounts)]
pub struct CloseEscrow<'info> {
    /// Emergency committee, receives the closed escrow ATA's rent.
    #[account(
        mut,
        constraint = config.emergency_committee == authority.key() @ ErrorCode::UnauthorizedCaller
    )]
    pub authority: Signer<'info>,

    #[account(address = CONFIG_PDA)]
    pub config: Account<'info, Config>,

    /// CHECK: The handler requires it to hold the mint and belong to the escrow authority (require_token_account).
    #[account(mut)]
    pub escrow_ata: UncheckedAccount<'info>,

    /// CHECK: The escrow authority, verified by address constraint; it signs the drain and the close.
    #[account(address = ESCROW_AUTHORITY @ ErrorCode::InvalidEscrowAuthority)]
    pub escrow_authority: UncheckedAccount<'info>,

    /// CHECK: Passed to the SPL Token transfer CPI.
    #[account(mut)]
    pub recipient_ata: UncheckedAccount<'info>,

    /// CHECK: The mint being drained; the handler requires escrow_ata to hold it.
    pub token_mint: UncheckedAccount<'info>,

    /// CHECK: Verified by address constraint.
    #[account(address = SPL_TOKEN_PROGRAM_ID)]
    pub token_program: UncheckedAccount<'info>,

    /// CHECK: Checked by require_paused_adapter in the handler.
    pub pa_state: UncheckedAccount<'info>,
}

#[derive(Accounts)]
pub struct CloseConfig<'info> {
    /// Emergency committee, receives the config's rent.
    #[account(mut)]
    pub authority: Signer<'info>,

    #[account(
        mut,
        address = CONFIG_PDA,
        constraint = config.emergency_committee == authority.key() @ ErrorCode::UnauthorizedCaller,
        close = authority
    )]
    pub config: Account<'info, Config>,

    /// CHECK: Checked by require_paused_adapter in the handler.
    pub pa_state: UncheckedAccount<'info>,
}

#[derive(Accounts)]
pub struct CloseNonceBitmaps<'info> {
    /// Emergency committee, receives the closed bitmaps' rent.
    #[account(
        mut,
        constraint = config.emergency_committee == authority.key() @ ErrorCode::UnauthorizedCaller
    )]
    pub authority: Signer<'info>,

    #[account(address = CONFIG_PDA)]
    pub config: Account<'info, Config>,

    /// CHECK: Checked by require_paused_adapter in the handler.
    pub pa_state: UncheckedAccount<'info>,
}
