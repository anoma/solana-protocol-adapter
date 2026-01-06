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

use anchor_lang::prelude::*;
use anchor_lang::solana_program::program::{invoke_signed, set_return_data};

mod ed25519;
mod error;
mod state;

pub use error::ErrorCode;
pub use state::*;

declare_id!("6cMwWUEoTnj8ManPCwAtXw5vdnp16mQKfUTdbxLNszN1");

/// Operation codes for forward_call input
pub const OP_WRAP: u8 = 0;
pub const OP_UNWRAP: u8 = 1;

/// Return values
pub const RESULT_SUCCESS: u8 = 1;
pub const RESULT_FAILURE: u8 = 0;

/// SPL Token program ID
const SPL_TOKEN_PROGRAM_ID: Pubkey = Pubkey::new_from_array([
    0x06, 0xdd, 0xf6, 0xe1, 0xd7, 0x65, 0xa1, 0x93, 0xd9, 0xcb, 0xe1, 0x46, 0xce, 0xeb, 0x79, 0xac,
    0x1c, 0xb4, 0x85, 0xed, 0x5f, 0x5b, 0x37, 0x91, 0x3a, 0x8c, 0xf5, 0x85, 0x7e, 0xff, 0x00, 0xa9,
]);

#[program]
pub mod spl_token_forwarder {
    use super::*;

    /// Initialize the forwarder config with emergency committee.
    pub fn initialize(
        ctx: Context<Initialize>,
        protocol_adapter: Pubkey,
        logic_ref: [u8; 32],
        emergency_committee: Pubkey,
    ) -> Result<()> {
        let config = &mut ctx.accounts.config;
        config.protocol_adapter = protocol_adapter;
        config.logic_ref = logic_ref;
        config.emergency_committee = emergency_committee;
        config.emergency_caller = Pubkey::default();
        config.is_stopped = false;
        config.bump = ctx.bumps.config;

        msg!("SPLTokenForwarder: initialized");
        msg!("  protocol_adapter: {}", protocol_adapter);
        msg!("  logic_ref: {:?}", &logic_ref[..8]);
        msg!("  emergency_committee: {}", emergency_committee);

        Ok(())
    }

    /// Forward a wrap or unwrap call from the Protocol Adapter.
    pub fn forward_call(
        ctx: Context<ForwardCall>,
        logic_ref: [u8; 32],
        input: Vec<u8>,
    ) -> Result<()> {
        let config = &ctx.accounts.config;

        msg!("SPLTokenForwarder: forward_call invoked");

        // Security: Only handle our specific logic_ref
        require!(
            logic_ref == config.logic_ref,
            ErrorCode::UnauthorizedLogicRef
        );
        msg!("  logic_ref validated");

        // Check not emergency stopped
        require!(!config.is_stopped, ErrorCode::EmergencyStopped);

        // Parse operation code
        let op = *input.get(0).ok_or(ErrorCode::InvalidInput)?;
        msg!("  operation: {}", op);

        match op {
            OP_WRAP => execute_wrap(&ctx, &input[1..])?,
            OP_UNWRAP => execute_unwrap(&ctx, &input[1..])?,
            _ => return Err(ErrorCode::UnknownOperation.into()),
        }

        // Return success
        set_return_data(&[RESULT_SUCCESS]);
        msg!("  return_data set: [{}]", RESULT_SUCCESS);

        Ok(())
    }

    /// Forward an emergency call when PA is stopped.
    pub fn forward_emergency_call(
        ctx: Context<ForwardEmergencyCall>,
        input: Vec<u8>,
    ) -> Result<()> {
        msg!("ForwardEmergencyCall:");
        msg!("  caller: {}", ctx.accounts.caller.key());

        if input.is_empty() {
            return Err(ErrorCode::InvalidInput.into());
        }

        let op = input[0];
        msg!("  operation: {}", op);

        match op {
            0 => execute_emergency_withdraw(&ctx, &input[1..]),
            _ => Err(ErrorCode::UnknownOperation.into()),
        }
    }

    /// Set the emergency caller (one-time, by emergency committee when stopped).
    pub fn set_emergency_caller(
        ctx: Context<SetEmergencyCaller>,
        new_emergency_caller: Pubkey,
    ) -> Result<()> {
        let config = &mut ctx.accounts.config;

        msg!("SetEmergencyCaller:");
        msg!("  committee: {}", ctx.accounts.committee.key());
        msg!("  new_emergency_caller: {}", new_emergency_caller);

        require!(
            new_emergency_caller != Pubkey::default(),
            ErrorCode::ZeroAddressNotAllowed
        );

        require!(
            config.emergency_caller == Pubkey::default(),
            ErrorCode::EmergencyCallerAlreadySet
        );

        config.emergency_caller = new_emergency_caller;
        msg!("Emergency caller set successfully");

        Ok(())
    }

    /// Emergency stop - sets is_stopped to true (by emergency committee).
    pub fn emergency_stop(ctx: Context<EmergencyStop>) -> Result<()> {
        let config = &mut ctx.accounts.config;

        msg!("EmergencyStop:");
        msg!("  committee: {}", ctx.accounts.committee.key());

        require!(!config.is_stopped, ErrorCode::AlreadyStopped);

        config.is_stopped = true;
        msg!("Forwarder emergency stopped");

        Ok(())
    }
}

// =============================================================================
// Wrap Operation
// =============================================================================

fn execute_wrap(ctx: &Context<ForwardCall>, input: &[u8]) -> Result<()> {
    let wrap_input = WrapInput::try_from_bytes(input)?;

    msg!("Wrap operation:");
    msg!("  token_mint: {}", wrap_input.token_mint);
    msg!("  amount: {}", wrap_input.amount);
    msg!("  user: {}", wrap_input.user);
    msg!("  nonce: {}", wrap_input.nonce);
    msg!("  deadline: {}", wrap_input.deadline);

    // Check deadline
    let clock = &ctx.accounts.clock;
    if clock.unix_timestamp > wrap_input.deadline {
        msg!("Deadline expired: current={}, deadline={}", clock.unix_timestamp, wrap_input.deadline);
        return Err(ErrorCode::DeadlineExpired.into());
    }
    msg!("  deadline check passed");

    // Verify Ed25519 signature via instruction introspection
    let message = wrap_input.to_message();
    let message_hash = message.hash();

    ed25519::verify_ed25519_instruction(
        &ctx.accounts.ix_sysvar,
        wrap_input.ed25519_ix_index,
        &wrap_input.user.to_bytes(),
        &message_hash,
    )?;
    msg!("  signature verified");

    // Extract remaining accounts
    let remaining = &ctx.remaining_accounts;
    if remaining.len() < 8 {
        msg!("Expected 8 remaining accounts, got {}", remaining.len());
        return Err(ErrorCode::InvalidInput.into());
    }

    let user_ata = &remaining[0];
    let escrow_ata = &remaining[1];
    let escrow_pda = &remaining[2];
    let nonce_pda = &remaining[3];
    let token_program = &remaining[4];
    let system_program = &remaining[5];
    let payer = &remaining[6];
    let _token_mint = &remaining[7];

    // Verify token program
    require!(token_program.key() == SPL_TOKEN_PROGRAM_ID, ErrorCode::InvalidInput);

    // Verify escrow PDA derivation
    let (expected_escrow_pda, escrow_bump) = Pubkey::find_program_address(
        &[ESCROW_SEED, wrap_input.token_mint.as_ref()],
        ctx.program_id,
    );
    require!(escrow_pda.key() == expected_escrow_pda, ErrorCode::InvalidEscrowPda);
    msg!("  escrow PDA verified");

    // Verify nonce PDA derivation and check it doesn't exist
    let nonce_bytes = wrap_input.nonce.to_le_bytes();
    let (expected_nonce_pda, nonce_bump) = Pubkey::find_program_address(
        &[NONCE_SEED, wrap_input.user.as_ref(), &nonce_bytes],
        ctx.program_id,
    );
    require!(nonce_pda.key() == expected_nonce_pda, ErrorCode::InvalidNoncePda);

    // Check nonce not already used
    if nonce_pda.data_len() > 0 && nonce_pda.owner == ctx.program_id {
        msg!("Nonce {} already used for user {}", wrap_input.nonce, wrap_input.user);
        return Err(ErrorCode::NonceAlreadyUsed.into());
    }
    msg!("  nonce not used");

    // Transfer tokens from user to escrow using delegate authority
    let mut transfer_data = vec![3u8]; // Transfer opcode
    transfer_data.extend_from_slice(&wrap_input.amount.to_le_bytes());

    let transfer_accounts = vec![
        AccountMeta::new(*user_ata.key, false),
        AccountMeta::new(*escrow_ata.key, false),
        AccountMeta::new_readonly(*escrow_pda.key, true),
    ];

    let transfer_ix = anchor_lang::solana_program::instruction::Instruction {
        program_id: SPL_TOKEN_PROGRAM_ID,
        accounts: transfer_accounts,
        data: transfer_data,
    };

    let escrow_seeds = &[ESCROW_SEED, wrap_input.token_mint.as_ref(), &[escrow_bump]];
    let signer_seeds = &[&escrow_seeds[..]];

    invoke_signed(
        &transfer_ix,
        &[
            user_ata.to_account_info(),
            escrow_ata.to_account_info(),
            escrow_pda.to_account_info(),
            token_program.to_account_info(),
        ],
        signer_seeds,
    ).map_err(|e| {
        msg!("Token transfer failed: {:?}", e);
        ErrorCode::TokenTransferFailed
    })?;
    msg!("  transferred {} tokens to escrow", wrap_input.amount);

    // Create nonce marker PDA to prevent replay
    let nonce_seeds = &[NONCE_SEED, wrap_input.user.as_ref(), &nonce_bytes, &[nonce_bump]];
    let nonce_signer_seeds = &[&nonce_seeds[..]];

    let rent = Rent::get()?;
    let lamports = rent.minimum_balance(1);

    let create_account_ix = anchor_lang::solana_program::system_instruction::create_account(
        payer.key,
        nonce_pda.key,
        lamports,
        1,
        ctx.program_id,
    );

    invoke_signed(
        &create_account_ix,
        &[
            payer.to_account_info(),
            nonce_pda.to_account_info(),
            system_program.to_account_info(),
        ],
        nonce_signer_seeds,
    )?;

    // Write marker byte
    let mut nonce_data = nonce_pda.try_borrow_mut_data()?;
    nonce_data[0] = 1;

    msg!("  nonce marker created");
    msg!("Wrap complete");

    Ok(())
}

// =============================================================================
// Unwrap Operation
// =============================================================================

fn execute_unwrap(ctx: &Context<ForwardCall>, input: &[u8]) -> Result<()> {
    let unwrap_input = UnwrapInput::try_from_bytes(input)?;

    msg!("Unwrap operation:");
    msg!("  token_mint: {}", unwrap_input.token_mint);
    msg!("  amount: {}", unwrap_input.amount);
    msg!("  recipient: {}", unwrap_input.recipient);

    let remaining = &ctx.remaining_accounts;
    if remaining.len() < 5 {
        msg!("Expected 5 remaining accounts, got {}", remaining.len());
        return Err(ErrorCode::InvalidInput.into());
    }

    let escrow_ata = &remaining[0];
    let recipient_ata = &remaining[1];
    let escrow_pda = &remaining[2];
    let token_program = &remaining[3];
    let _token_mint = &remaining[4];

    require!(token_program.key() == SPL_TOKEN_PROGRAM_ID, ErrorCode::InvalidInput);

    // Verify escrow PDA
    let (expected_escrow_pda, escrow_bump) = Pubkey::find_program_address(
        &[ESCROW_SEED, unwrap_input.token_mint.as_ref()],
        ctx.program_id,
    );
    require!(escrow_pda.key() == expected_escrow_pda, ErrorCode::InvalidEscrowPda);
    msg!("  escrow PDA verified");

    // Transfer tokens from escrow to recipient
    let mut transfer_data = vec![3u8];
    transfer_data.extend_from_slice(&unwrap_input.amount.to_le_bytes());

    let transfer_accounts = vec![
        AccountMeta::new(*escrow_ata.key, false),
        AccountMeta::new(*recipient_ata.key, false),
        AccountMeta::new_readonly(*escrow_pda.key, true),
    ];

    let transfer_ix = anchor_lang::solana_program::instruction::Instruction {
        program_id: SPL_TOKEN_PROGRAM_ID,
        accounts: transfer_accounts,
        data: transfer_data,
    };

    let escrow_seeds = &[ESCROW_SEED, unwrap_input.token_mint.as_ref(), &[escrow_bump]];
    let signer_seeds = &[&escrow_seeds[..]];

    invoke_signed(
        &transfer_ix,
        &[
            escrow_ata.to_account_info(),
            recipient_ata.to_account_info(),
            escrow_pda.to_account_info(),
            token_program.to_account_info(),
        ],
        signer_seeds,
    ).map_err(|e| {
        msg!("Token transfer failed: {:?}", e);
        ErrorCode::InsufficientEscrowBalance
    })?;

    msg!("  transferred {} tokens to recipient", unwrap_input.amount);
    msg!("Unwrap complete");

    Ok(())
}

// =============================================================================
// Emergency Withdraw
// =============================================================================

fn execute_emergency_withdraw(ctx: &Context<ForwardEmergencyCall>, input: &[u8]) -> Result<()> {
    if input.len() != 72 {
        msg!("Emergency withdraw input must be 72 bytes, got {}", input.len());
        return Err(ErrorCode::InvalidInput.into());
    }

    let token_mint = Pubkey::new_from_array(input[0..32].try_into().unwrap());
    let amount = u64::from_le_bytes(input[32..40].try_into().unwrap());
    let recipient = Pubkey::new_from_array(input[40..72].try_into().unwrap());

    msg!("Emergency withdraw:");
    msg!("  token_mint: {}", token_mint);
    msg!("  amount: {}", amount);
    msg!("  recipient: {}", recipient);

    let remaining = &ctx.remaining_accounts;
    if remaining.len() < 4 {
        msg!("Expected 4 remaining accounts, got {}", remaining.len());
        return Err(ErrorCode::InvalidInput.into());
    }

    let escrow_ata = &remaining[0];
    let recipient_ata = &remaining[1];
    let escrow_pda = &remaining[2];
    let token_program = &remaining[3];

    require!(token_program.key() == SPL_TOKEN_PROGRAM_ID, ErrorCode::InvalidInput);

    // Verify escrow PDA
    let (expected_escrow_pda, escrow_bump) = Pubkey::find_program_address(
        &[ESCROW_SEED, token_mint.as_ref()],
        ctx.program_id,
    );
    require!(escrow_pda.key() == expected_escrow_pda, ErrorCode::InvalidEscrowPda);

    // Transfer tokens
    let mut transfer_data = vec![3u8];
    transfer_data.extend_from_slice(&amount.to_le_bytes());

    let transfer_accounts = vec![
        AccountMeta::new(*escrow_ata.key, false),
        AccountMeta::new(*recipient_ata.key, false),
        AccountMeta::new_readonly(*escrow_pda.key, true),
    ];

    let transfer_ix = anchor_lang::solana_program::instruction::Instruction {
        program_id: SPL_TOKEN_PROGRAM_ID,
        accounts: transfer_accounts,
        data: transfer_data,
    };

    let escrow_seeds = &[ESCROW_SEED, token_mint.as_ref(), &[escrow_bump]];
    let signer_seeds = &[&escrow_seeds[..]];

    invoke_signed(
        &transfer_ix,
        &[
            escrow_ata.to_account_info(),
            recipient_ata.to_account_info(),
            escrow_pda.to_account_info(),
            token_program.to_account_info(),
        ],
        signer_seeds,
    )?;

    msg!("Emergency withdraw complete");
    Ok(())
}

// =============================================================================
// Account Contexts
// =============================================================================

#[derive(Accounts)]
pub struct Initialize<'info> {
    #[account(mut)]
    pub authority: Signer<'info>,

    #[account(
        init,
        payer = authority,
        space = 8 + Config::INIT_SPACE,
        seeds = [b"config"],
        bump
    )]
    pub config: Account<'info, Config>,

    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct ForwardCall<'info> {
    /// The caller must be the Protocol Adapter
    /// CHECK: Verified via constraint against config.protocol_adapter
    #[account(
        constraint = caller.key() == config.protocol_adapter @ ErrorCode::UnauthorizedCaller
    )]
    pub caller: AccountInfo<'info>,

    #[account(
        seeds = [b"config"],
        bump = config.bump
    )]
    pub config: Account<'info, Config>,

    /// Instructions sysvar for Ed25519 signature introspection
    /// CHECK: Validated via address constraint
    #[account(address = anchor_lang::solana_program::sysvar::instructions::ID)]
    pub ix_sysvar: AccountInfo<'info>,

    /// Clock sysvar for deadline checking
    pub clock: Sysvar<'info, Clock>,
}

#[derive(Accounts)]
pub struct ForwardEmergencyCall<'info> {
    #[account(mut)]
    pub caller: Signer<'info>,

    #[account(
        seeds = [b"config"],
        bump = config.bump,
        constraint = config.is_stopped @ ErrorCode::ProtocolAdapterNotStopped,
        constraint = config.emergency_caller == caller.key() @ ErrorCode::UnauthorizedCaller
    )]
    pub config: Account<'info, Config>,
}

#[derive(Accounts)]
pub struct SetEmergencyCaller<'info> {
    pub committee: Signer<'info>,

    #[account(
        mut,
        seeds = [b"config"],
        bump = config.bump,
        constraint = config.is_stopped @ ErrorCode::ProtocolAdapterNotStopped,
        constraint = config.emergency_committee == committee.key() @ ErrorCode::UnauthorizedCaller
    )]
    pub config: Account<'info, Config>,
}

#[derive(Accounts)]
pub struct EmergencyStop<'info> {
    pub committee: Signer<'info>,

    #[account(
        mut,
        seeds = [b"config"],
        bump = config.bump,
        constraint = config.emergency_committee == committee.key() @ ErrorCode::UnauthorizedCaller
    )]
    pub config: Account<'info, Config>,
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_operation_constants() {
        assert_eq!(OP_WRAP, 0);
        assert_eq!(OP_UNWRAP, 1);
    }

    #[test]
    fn test_result_constants() {
        assert_eq!(RESULT_SUCCESS, 1);
        assert_eq!(RESULT_FAILURE, 0);
    }

    #[test]
    fn test_spl_token_program_id() {
        use std::str::FromStr;
        let expected = Pubkey::from_str("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA").unwrap();
        assert_eq!(SPL_TOKEN_PROGRAM_ID, expected);
    }
}
