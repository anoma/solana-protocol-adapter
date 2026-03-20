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

// =============================================================================
// Conditional Debug Logging
// =============================================================================

/// Debug logging macro - only emits logs when `verbose-logging` feature is enabled.
///
/// Use for:
/// - Field-by-field dumps (token_mint, amount, user, etc.)
/// - Step-by-step progress messages ("deadline check passed", etc.)
/// - Balance before/after details
///
/// Regular `msg!()` should be used for:
/// - Error diagnostics (always useful for debugging failures)
/// - Operation completion ("Wrap complete", "Unwrap complete")
/// - Critical state transitions
#[cfg(feature = "verbose-logging")]
macro_rules! debug_msg {
    ($($arg:tt)*) => {
        msg!($($arg)*)
    };
}

#[cfg(not(feature = "verbose-logging"))]
macro_rules! debug_msg {
    ($($arg:tt)*) => {};
}

pub mod ed25519;
mod error;
pub mod state;
#[cfg(test)]
mod tests;

pub use error::ErrorCode;
pub use state::*;

declare_id!("DuBnfWgTVCfFYcjZGakNTd2A7zFX6X4eAEDxVAkRrZCD");

// =============================================================================
// Events (mirrors EVM: Wrapped, Unwrapped events)
// =============================================================================

/// Emitted when tokens are wrapped (locked into escrow).
///
/// Mirrors EVM: `event Wrapped(address indexed token, address indexed from, uint128 amount);`
#[event]
pub struct Wrapped {
    /// The SPL token mint that was wrapped
    pub token_mint: Pubkey,
    /// The user who wrapped tokens (signed the authorization)
    pub from: Pubkey,
    /// The amount of tokens wrapped
    pub amount: u64,
    /// The nonce used for replay protection
    pub nonce: u64,
    /// The action tree root this wrap was bound to
    pub action_tree_root: [u8; 32],
}

/// Emitted when tokens are unwrapped (released from escrow).
///
/// Mirrors EVM: `event Unwrapped(address indexed token, address indexed to, uint128 amount);`
#[event]
pub struct Unwrapped {
    /// The SPL token mint that was unwrapped
    pub token_mint: Pubkey,
    /// The recipient who received the tokens
    pub to: Pubkey,
    /// The amount of tokens unwrapped
    pub amount: u64,
}

/// Emitted when emergency caller is set by committee.
///
/// Mirrors EVM: `event EmergencyCallerSet(address caller);`
#[event]
pub struct EmergencyCallerSet {
    /// The new emergency caller address
    pub emergency_caller: Pubkey,
    /// The committee that set this caller
    pub set_by: Pubkey,
}

/// Emitted when emergency withdrawal is performed.
#[event]
pub struct EmergencyWithdraw {
    /// The SPL token mint that was withdrawn
    pub token_mint: Pubkey,
    /// The recipient who received the tokens
    pub to: Pubkey,
    /// The amount of tokens withdrawn
    pub amount: u64,
    /// The emergency caller who performed the withdrawal
    pub caller: Pubkey,
}

/// Operation codes for forward_call input
pub const OP_WRAP: u8 = 0;
pub const OP_UNWRAP: u8 = 1;

/// Return value for successful forward_call
pub const RESULT_SUCCESS: u8 = 1;

/// Emergency withdraw operation code (used in forward_emergency_call)
pub const OP_EMERGENCY_WITHDRAW: u8 = 0;

/// SPL Token "Transfer" instruction opcode
const SPL_TRANSFER_OPCODE: u8 = 3;

/// SPL Token program ID
const SPL_TOKEN_PROGRAM_ID: Pubkey = Pubkey::new_from_array([
    0x06, 0xdd, 0xf6, 0xe1, 0xd7, 0x65, 0xa1, 0x93, 0xd9, 0xcb, 0xe1, 0x46, 0xce, 0xeb, 0x79, 0xac,
    0x1c, 0xb4, 0x85, 0xed, 0x5f, 0x5b, 0x37, 0x91, 0x3a, 0x8c, 0xf5, 0x85, 0x7e, 0xff, 0x00, 0xa9,
]);

#[program]
pub mod spl_token_forwarder {
    use super::*;

    /// Initialize the forwarder config with emergency committee.
    ///
    /// Mirrors EVM constructor validation:
    /// - protocol_adapter must not be zero
    /// - logic_ref must not be all zeros
    /// - emergency_committee must not be zero
    pub fn initialize(
        ctx: Context<Initialize>,
        protocol_adapter: Pubkey,
        logic_ref: [u8; 32],
        emergency_committee: Pubkey,
    ) -> Result<()> {
        // Mirrors: test_constructor_reverts_if_the_protocol_adapter_address_is_zero
        require!(
            protocol_adapter != Pubkey::default(),
            ErrorCode::ZeroAddressNotAllowed
        );

        // Mirrors: test_constructor_reverts_if_the_logic_ref_is_zero
        require!(logic_ref != [0u8; 32], ErrorCode::ZeroAddressNotAllowed);

        // Mirrors: test_constructor_reverts_if_the_emergency_committe_address_is_zero
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

        msg!("SPLTokenForwarder: initialized");
        debug_msg!("  protocol_adapter: {}", protocol_adapter);
        debug_msg!("  logic_ref: {:?}", &logic_ref[..8]);
        debug_msg!("  emergency_committee: {}", emergency_committee);

        Ok(())
    }

    /// Forward a wrap or unwrap call from the Protocol Adapter.
    ///
    /// Note: Unlike EVM which doesn't check stopped state here (trusting PA won't call if stopped),
    /// we also don't check here. The PA is responsible for not calling forwarders when stopped.
    /// This matches the EVM ForwarderBase.forwardCall() pattern exactly.
    pub fn forward_call(
        ctx: Context<ForwardCall>,
        logic_ref: [u8; 32],
        input: Vec<u8>,
    ) -> Result<()> {
        let config = &ctx.accounts.config;

        msg!("SPLTokenForwarder: forward_call invoked");

        // Security: Verify this call came via CPI from the Protocol Adapter.
        // The instructions sysvar stores only top-level instructions. During CPI,
        // the current instruction index points to the outer (calling) instruction.
        // If called directly, the program_id will be the forwarder itself — rejected.
        // This mirrors EVM's msg.sender == _PROTOCOL_ADAPTER check.
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
        debug_msg!("  CPI caller verified: {}", config.protocol_adapter);

        // Security: Only handle our specific logic_ref
        require!(
            logic_ref == config.logic_ref,
            ErrorCode::UnauthorizedLogicRef
        );
        debug_msg!("  logic_ref validated");

        // Parse operation code
        let op = *input.first().ok_or(ErrorCode::InvalidInput)?;
        debug_msg!("  operation: {}", op);

        match op {
            OP_WRAP => execute_wrap(&ctx, &input[1..])?,
            OP_UNWRAP => execute_unwrap(&ctx, &input[1..])?,
            _ => return Err(ErrorCode::UnknownOperation.into()),
        }

        // Return success
        set_return_data(&[RESULT_SUCCESS]);
        debug_msg!("  return_data set: [{}]", RESULT_SUCCESS);

        Ok(())
    }

    /// Forward an emergency call when PA is stopped.
    ///
    /// Mirrors EVM's forwardEmergencyCall() which checks _checkEmergencyStopped()
    /// by querying ProtocolAdapter.isEmergencyStopped().
    pub fn forward_emergency_call(
        ctx: Context<ForwardEmergencyCall>,
        input: Vec<u8>,
    ) -> Result<()> {
        msg!("ForwardEmergencyCall");
        debug_msg!("  caller: {}", ctx.accounts.caller.key());

        // Verify PA is emergency stopped (mirrors EVM _checkEmergencyStopped)
        let pa_state_data = ctx.accounts.pa_state.try_borrow_data()?;
        require!(
            is_pa_emergency_stopped(&pa_state_data),
            ErrorCode::ProtocolAdapterNotStopped
        );
        drop(pa_state_data);
        debug_msg!("  PA emergency stopped: verified");

        // Verify emergency caller is set (mirrors EVM EmergencyCallerNotSet check)
        require!(
            ctx.accounts.config.emergency_caller != Pubkey::default(),
            ErrorCode::EmergencyCallerNotSet
        );

        // Verify caller is the authorized emergency caller
        require!(
            ctx.accounts.config.emergency_caller == ctx.accounts.caller.key(),
            ErrorCode::UnauthorizedCaller
        );

        if input.is_empty() {
            return Err(ErrorCode::InvalidInput.into());
        }

        let op = input[0];
        debug_msg!("  operation: {}", op);

        match op {
            OP_EMERGENCY_WITHDRAW => execute_emergency_withdraw(&ctx, &input[1..]),
            _ => Err(ErrorCode::UnknownOperation.into()),
        }
    }

    /// Set the emergency caller (one-time, by emergency committee when PA is stopped).
    ///
    /// Mirrors EVM's setEmergencyCaller() which checks _checkEmergencyStopped()
    /// by querying ProtocolAdapter.isEmergencyStopped().
    pub fn set_emergency_caller(
        ctx: Context<SetEmergencyCaller>,
        new_emergency_caller: Pubkey,
    ) -> Result<()> {
        let config = &mut ctx.accounts.config;

        msg!("SetEmergencyCaller");
        debug_msg!("  committee: {}", ctx.accounts.committee.key());
        debug_msg!("  new_emergency_caller: {}", new_emergency_caller);

        // Verify PA is emergency stopped (mirrors EVM _checkEmergencyStopped)
        let pa_state_data = ctx.accounts.pa_state.try_borrow_data()?;
        require!(
            is_pa_emergency_stopped(&pa_state_data),
            ErrorCode::ProtocolAdapterNotStopped
        );
        drop(pa_state_data);
        debug_msg!("  PA emergency stopped: verified");

        require!(
            new_emergency_caller != Pubkey::default(),
            ErrorCode::ZeroAddressNotAllowed
        );

        require!(
            config.emergency_caller == Pubkey::default(),
            ErrorCode::EmergencyCallerAlreadySet
        );

        config.emergency_caller = new_emergency_caller;

        // Emit structured event (mirrors EVM EmergencyCallerSet event)
        emit!(EmergencyCallerSet {
            emergency_caller: new_emergency_caller,
            set_by: ctx.accounts.committee.key(),
        });
        msg!("Emergency caller set successfully");

        Ok(())
    }
}

// =============================================================================
// Balance Reading (for fee-on-transfer token safety)
// =============================================================================

/// Offset of the amount field in SPL Token account data.
/// Layout: mint(32) + owner(32) + amount(8) = offset 64
const TOKEN_ACCOUNT_AMOUNT_OFFSET: usize = 64;

/// Read the balance from an SPL token account.
///
/// Mirrors EVM's token.balanceOf(address) for before/after balance verification.
fn read_token_balance(token_account: &AccountInfo) -> Result<u64> {
    let data = token_account.try_borrow_data()?;
    if data.len() < TOKEN_ACCOUNT_AMOUNT_OFFSET + 8 {
        msg!(
            "Invalid token account data length: {} (expected >= {})",
            data.len(),
            TOKEN_ACCOUNT_AMOUNT_OFFSET + 8
        );
        return Err(ErrorCode::InvalidTokenAccountData.into());
    }
    let amount_bytes: [u8; 8] = data[TOKEN_ACCOUNT_AMOUNT_OFFSET..TOKEN_ACCOUNT_AMOUNT_OFFSET + 8]
        .try_into()
        .unwrap();
    Ok(u64::from_le_bytes(amount_bytes))
}

// =============================================================================
// SPL Transfer Helper
// =============================================================================

/// Build an SPL Token Transfer instruction.
///
/// Used by wrap (user→escrow), unwrap (escrow→recipient), and emergency withdraw.
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

// =============================================================================
// Wrap Operation
// =============================================================================

fn execute_wrap(ctx: &Context<ForwardCall>, input: &[u8]) -> Result<()> {
    let wrap_input = WrapInput::try_from_bytes(input)?;

    msg!("Wrap operation");
    debug_msg!("  token_mint: {}", wrap_input.token_mint);
    debug_msg!("  amount: {}", wrap_input.amount);
    debug_msg!("  user: {}", wrap_input.user);
    debug_msg!("  nonce: {}", wrap_input.nonce);
    debug_msg!("  deadline: {}", wrap_input.deadline);

    // Check deadline per Permit2/EIP-2612 semantics: allow execution AT deadline, reject after.
    // - Permit2: `if (block.timestamp > permit.deadline) revert`
    //   https://github.com/Uniswap/permit2/blob/main/src/SignatureTransfer.sol
    // - EIP-2612: `require(block.timestamp <= deadline)`
    //   https://eips.ethereum.org/EIPS/eip-2612
    let clock = &ctx.accounts.clock;
    if clock.unix_timestamp > wrap_input.deadline {
        msg!(
            "Deadline expired: current={}, deadline={}",
            clock.unix_timestamp,
            wrap_input.deadline
        );
        return Err(ErrorCode::DeadlineExpired.into());
    }
    debug_msg!("  deadline check passed");

    // Extract remaining accounts early for nonce check (DoS prevention: check cheap
    // conditions before expensive Ed25519 signature verification)
    let remaining = &ctx.remaining_accounts;
    if remaining.len() < 8 {
        msg!(
            "Expected 8 remaining accounts for wrap, got {}",
            remaining.len()
        );
        return Err(ErrorCode::InsufficientRemainingAccounts.into());
    }

    let user_ata = &remaining[0];
    let escrow_ata = &remaining[1];
    let escrow_pda = &remaining[2];
    let nonce_bitmap_pda = &remaining[3];
    let token_program = &remaining[4];
    let system_program = &remaining[5];
    let payer = &remaining[6];
    let token_mint_account = &remaining[7];

    // Verify nonce bitmap PDA derivation and check nonce BEFORE signature verification
    // (Permit2-style bitmap pattern - rejects replays without wasting compute on sig verify)
    let (word_index, bit_position) = nonce_to_word_and_bit(wrap_input.nonce);
    let (expected_bitmap_pda, bitmap_bump) =
        derive_nonce_bitmap_pda(ctx.program_id, &wrap_input.user, word_index);
    require!(
        nonce_bitmap_pda.key() == expected_bitmap_pda,
        ErrorCode::InvalidNonceBitmapPda
    );
    debug_msg!(
        "  nonce bitmap PDA verified (word={}, bit={})",
        word_index,
        bit_position
    );

    // Check if bitmap exists and if nonce bit is already set
    let bitmap_exists = nonce_bitmap_pda.data_len() >= NONCE_BITMAP_SIZE
        && nonce_bitmap_pda.owner == ctx.program_id;

    if bitmap_exists {
        let bitmap_data = nonce_bitmap_pda.try_borrow_data()?;
        if is_nonce_used(&bitmap_data, bit_position) {
            msg!(
                "Nonce {} already used for user {}",
                wrap_input.nonce,
                wrap_input.user
            );
            return Err(ErrorCode::NonceAlreadyUsed.into());
        }
        drop(bitmap_data);
    }
    debug_msg!("  nonce {} not used", wrap_input.nonce);

    // Verify Ed25519 signature via instruction introspection.
    // The user signs base64(sha256(120-byte WrapMessage)) — 44 bytes of valid UTF-8.
    // Wallets reject raw binary in signMessage as a potential transaction.
    // Base64 is compact (+12 bytes over raw hash) and fits in tx limits.
    // Include program ID in message for domain separation (mirrors EIP-712).
    let message = wrap_input.to_message(ctx.program_id);
    let message_hash = message.hash();

    // Base64-encode the 32-byte hash → 44 bytes of ASCII text.
    // Standard base64: each 3 bytes → 4 chars, 32 bytes → ceil(32/3)*4 = 44 chars.
    const BASE64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut b64_bytes = [0u8; 44];
    let mut si = 0;
    let mut di = 0;
    while si + 2 < 32 {
        let (a, b, c) = (message_hash[si] as u32, message_hash[si + 1] as u32, message_hash[si + 2] as u32);
        let triple = (a << 16) | (b << 8) | c;
        b64_bytes[di] = BASE64[((triple >> 18) & 0x3F) as usize];
        b64_bytes[di + 1] = BASE64[((triple >> 12) & 0x3F) as usize];
        b64_bytes[di + 2] = BASE64[((triple >> 6) & 0x3F) as usize];
        b64_bytes[di + 3] = BASE64[(triple & 0x3F) as usize];
        si += 3;
        di += 4;
    }
    // Handle last 2 bytes (32 = 10*3 + 2): 2 remaining bytes → 3 chars + '='
    let (a, b) = (message_hash[si] as u32, message_hash[si + 1] as u32);
    let triple = (a << 16) | (b << 8);
    b64_bytes[di] = BASE64[((triple >> 18) & 0x3F) as usize];
    b64_bytes[di + 1] = BASE64[((triple >> 12) & 0x3F) as usize];
    b64_bytes[di + 2] = BASE64[((triple >> 6) & 0x3F) as usize];
    b64_bytes[di + 3] = b'=';

    ed25519::verify_ed25519_instruction(
        &ctx.accounts.ix_sysvar,
        wrap_input.ed25519_ix_index,
        &wrap_input.user.to_bytes(),
        &b64_bytes,
    )?;
    debug_msg!("  signature verified");

    // Verify token program
    require!(
        token_program.key() == SPL_TOKEN_PROGRAM_ID,
        ErrorCode::InvalidTokenProgram
    );

    // Verify token mint account matches input
    require!(
        token_mint_account.key() == wrap_input.token_mint,
        ErrorCode::TokenMintMismatch
    );
    debug_msg!("  token mint verified: {}", wrap_input.token_mint);

    // Verify escrow PDA derivation
    let (expected_escrow_pda, escrow_bump) = Pubkey::find_program_address(
        &[ESCROW_SEED, wrap_input.token_mint.as_ref()],
        ctx.program_id,
    );
    require!(
        escrow_pda.key() == expected_escrow_pda,
        ErrorCode::InvalidEscrowPda
    );
    debug_msg!("  escrow PDA verified");

    // Validate delegate approval before attempting transfer
    // SPL Token account layout: mint(32) + owner(32) + amount(8) + delegate_option(4) + delegate(32) + state(1) + is_native_option(4) + is_native(8) + delegated_amount(8)
    let user_ata_data = user_ata.try_borrow_data()?;
    if user_ata_data.len() < 129 {
        msg!(
            "Invalid token account data length: {} (expected >= 129)",
            user_ata_data.len()
        );
        return Err(ErrorCode::InvalidTokenAccountData.into());
    }

    // Check delegate option (offset 72): 1 = Some, 0 = None
    let delegate_option = u32::from_le_bytes(user_ata_data[72..76].try_into().unwrap());
    if delegate_option != 1 {
        msg!("No delegate set on user token account - user must approve escrow PDA first");
        return Err(ErrorCode::InsufficientDelegateApproval.into());
    }

    // Check delegate pubkey (offset 76-108)
    let delegate_bytes: [u8; 32] = user_ata_data[76..108].try_into().unwrap();
    let delegate_pubkey = Pubkey::new_from_array(delegate_bytes);
    if delegate_pubkey != escrow_pda.key() {
        msg!(
            "Delegate mismatch: expected escrow PDA {}, got {}",
            escrow_pda.key(),
            delegate_pubkey
        );
        return Err(ErrorCode::InsufficientDelegateApproval.into());
    }

    // Check delegated amount (offset 121-129)
    let delegated_amount = u64::from_le_bytes(user_ata_data[121..129].try_into().unwrap());
    if delegated_amount < wrap_input.amount {
        msg!(
            "Insufficient delegated amount: have {}, need {}",
            delegated_amount,
            wrap_input.amount
        );
        return Err(ErrorCode::InsufficientDelegateApproval.into());
    }
    debug_msg!(
        "  delegate approval verified: {} >= {}",
        delegated_amount,
        wrap_input.amount
    );

    drop(user_ata_data); // Release borrow before CPI

    // Read escrow balance before transfer (mirrors EVM balanceBefore pattern)
    let escrow_balance_before = read_token_balance(escrow_ata)?;
    debug_msg!("  escrow balance before: {}", escrow_balance_before);

    // Transfer tokens from user to escrow using delegate authority
    let transfer_ix = spl_transfer_ix(user_ata.key, escrow_ata.key, escrow_pda.key, wrap_input.amount);

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
    )
    .map_err(|e| {
        msg!("Token transfer failed: {:?}", e);
        ErrorCode::TokenTransferFailed
    })?;

    // Read escrow balance after transfer and verify delta (mirrors EVM balanceDelta pattern)
    let escrow_balance_after = read_token_balance(escrow_ata)?;
    let actual_delta = escrow_balance_after.saturating_sub(escrow_balance_before);
    debug_msg!("  escrow balance after: {}", escrow_balance_after);
    debug_msg!(
        "  actual delta: {}, expected: {}",
        actual_delta,
        wrap_input.amount
    );

    if actual_delta != wrap_input.amount {
        msg!(
            "Balance mismatch: expected {}, actual {}",
            wrap_input.amount,
            actual_delta
        );
        return Err(ErrorCode::BalanceMismatch.into());
    }
    debug_msg!("  balance verification passed");

    // Mark nonce as used in bitmap (Permit2-style pattern)
    // Each bitmap PDA stores 256 nonces, reducing account bloat by 256x
    if bitmap_exists {
        // Bitmap exists - just set the bit
        let mut bitmap_data = nonce_bitmap_pda.try_borrow_mut_data()?;
        set_nonce_used(&mut bitmap_data, bit_position);
        debug_msg!(
            "  nonce {} marked in existing bitmap (word={})",
            wrap_input.nonce,
            word_index
        );
    } else {
        // Bitmap doesn't exist - create it with this nonce set
        let rent = Rent::get()?;
        let lamports = rent.minimum_balance(NONCE_BITMAP_SIZE);

        let word_bytes = word_index.to_le_bytes();
        let bitmap_seeds = &[
            NONCE_BITMAP_SEED,
            wrap_input.user.as_ref(),
            &word_bytes,
            &[bitmap_bump],
        ];
        let bitmap_signer_seeds = &[&bitmap_seeds[..]];

        let create_account_ix = anchor_lang::solana_program::system_instruction::create_account(
            payer.key,
            nonce_bitmap_pda.key,
            lamports,
            NONCE_BITMAP_SIZE as u64,
            ctx.program_id,
        );

        invoke_signed(
            &create_account_ix,
            &[
                payer.to_account_info(),
                nonce_bitmap_pda.to_account_info(),
                system_program.to_account_info(),
            ],
            bitmap_signer_seeds,
        )?;

        // Set the nonce bit in the newly created bitmap
        let mut bitmap_data = nonce_bitmap_pda.try_borrow_mut_data()?;
        set_nonce_used(&mut bitmap_data, bit_position);
        debug_msg!(
            "  created bitmap for word {} and marked nonce {}",
            word_index,
            wrap_input.nonce
        );
    }

    // Emit structured event (mirrors EVM Wrapped event)
    emit!(Wrapped {
        token_mint: wrap_input.token_mint,
        from: wrap_input.user,
        amount: wrap_input.amount,
        nonce: wrap_input.nonce,
        action_tree_root: wrap_input.action_tree_root,
    });
    msg!("Wrap complete");

    Ok(())
}

// =============================================================================
// Unwrap Operation
// =============================================================================

fn execute_unwrap(ctx: &Context<ForwardCall>, input: &[u8]) -> Result<()> {
    let unwrap_input = UnwrapInput::try_from_bytes(input)?;

    msg!("Unwrap operation");
    debug_msg!("  token_mint: {}", unwrap_input.token_mint);
    debug_msg!("  amount: {}", unwrap_input.amount);
    debug_msg!("  recipient: {}", unwrap_input.recipient);

    let remaining = &ctx.remaining_accounts;
    if remaining.len() < 5 {
        msg!(
            "Expected 5 remaining accounts for unwrap, got {}",
            remaining.len()
        );
        return Err(ErrorCode::InsufficientRemainingAccounts.into());
    }

    let escrow_ata = &remaining[0];
    let recipient_ata = &remaining[1];
    let escrow_pda = &remaining[2];
    let token_program = &remaining[3];
    let token_mint_account = &remaining[4];

    require!(
        token_program.key() == SPL_TOKEN_PROGRAM_ID,
        ErrorCode::InvalidTokenProgram
    );

    // Verify token mint account matches input
    require!(
        token_mint_account.key() == unwrap_input.token_mint,
        ErrorCode::TokenMintMismatch
    );
    debug_msg!("  token mint verified: {}", unwrap_input.token_mint);

    // Verify escrow PDA
    let (expected_escrow_pda, escrow_bump) = Pubkey::find_program_address(
        &[ESCROW_SEED, unwrap_input.token_mint.as_ref()],
        ctx.program_id,
    );
    require!(
        escrow_pda.key() == expected_escrow_pda,
        ErrorCode::InvalidEscrowPda
    );
    debug_msg!("  escrow PDA verified");

    // Read recipient balance before transfer (mirrors EVM balanceBefore pattern)
    let recipient_balance_before = read_token_balance(recipient_ata)?;
    debug_msg!("  recipient balance before: {}", recipient_balance_before);

    // Transfer tokens from escrow to recipient
    let transfer_ix = spl_transfer_ix(escrow_ata.key, recipient_ata.key, escrow_pda.key, unwrap_input.amount);

    let escrow_seeds = &[
        ESCROW_SEED,
        unwrap_input.token_mint.as_ref(),
        &[escrow_bump],
    ];
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
    )
    .map_err(|e| {
        msg!("Token transfer failed: {:?}", e);
        ErrorCode::InsufficientEscrowBalance
    })?;

    // Read recipient balance after transfer and verify delta (mirrors EVM balanceDelta pattern)
    let recipient_balance_after = read_token_balance(recipient_ata)?;
    let actual_delta = recipient_balance_after.saturating_sub(recipient_balance_before);
    debug_msg!("  recipient balance after: {}", recipient_balance_after);
    debug_msg!(
        "  actual delta: {}, expected: {}",
        actual_delta,
        unwrap_input.amount
    );

    if actual_delta != unwrap_input.amount {
        msg!(
            "Balance mismatch: expected {}, actual {}",
            unwrap_input.amount,
            actual_delta
        );
        return Err(ErrorCode::BalanceMismatch.into());
    }
    debug_msg!("  balance verification passed");

    // Emit structured event (mirrors EVM Unwrapped event)
    emit!(Unwrapped {
        token_mint: unwrap_input.token_mint,
        to: unwrap_input.recipient,
        amount: unwrap_input.amount,
    });
    msg!("Unwrap complete");

    Ok(())
}

// =============================================================================
// Emergency Withdraw
// =============================================================================

fn execute_emergency_withdraw(ctx: &Context<ForwardEmergencyCall>, input: &[u8]) -> Result<()> {
    if input.len() != 72 {
        msg!(
            "Emergency withdraw input must be 72 bytes, got {}",
            input.len()
        );
        return Err(ErrorCode::InvalidEmergencyInputLength.into());
    }

    let token_mint = Pubkey::new_from_array(input[0..32].try_into().unwrap());
    let amount = u64::from_le_bytes(input[32..40].try_into().unwrap());
    let recipient = Pubkey::new_from_array(input[40..72].try_into().unwrap());

    msg!("Emergency withdraw");
    debug_msg!("  token_mint: {}", token_mint);
    debug_msg!("  amount: {}", amount);
    debug_msg!("  recipient: {}", recipient);

    let remaining = &ctx.remaining_accounts;
    if remaining.len() < 4 {
        msg!(
            "Expected 4 remaining accounts for emergency withdraw, got {}",
            remaining.len()
        );
        return Err(ErrorCode::InsufficientRemainingAccounts.into());
    }

    let escrow_ata = &remaining[0];
    let recipient_ata = &remaining[1];
    let escrow_pda = &remaining[2];
    let token_program = &remaining[3];

    require!(
        token_program.key() == SPL_TOKEN_PROGRAM_ID,
        ErrorCode::InvalidTokenProgram
    );

    // Verify escrow PDA
    let (expected_escrow_pda, escrow_bump) =
        Pubkey::find_program_address(&[ESCROW_SEED, token_mint.as_ref()], ctx.program_id);
    require!(
        escrow_pda.key() == expected_escrow_pda,
        ErrorCode::InvalidEscrowPda
    );

    // Read recipient balance before transfer (mirrors EVM balanceBefore pattern)
    let recipient_balance_before = read_token_balance(recipient_ata)?;
    debug_msg!("  recipient balance before: {}", recipient_balance_before);

    // Transfer tokens
    let transfer_ix = spl_transfer_ix(escrow_ata.key, recipient_ata.key, escrow_pda.key, amount);

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

    // Read recipient balance after transfer and verify delta (mirrors EVM balanceDelta pattern)
    let recipient_balance_after = read_token_balance(recipient_ata)?;
    let actual_delta = recipient_balance_after.saturating_sub(recipient_balance_before);
    debug_msg!("  recipient balance after: {}", recipient_balance_after);
    debug_msg!("  actual delta: {}, expected: {}", actual_delta, amount);

    if actual_delta != amount {
        msg!(
            "Balance mismatch: expected {}, actual {}",
            amount,
            actual_delta
        );
        return Err(ErrorCode::BalanceMismatch.into());
    }
    debug_msg!("  balance verification passed");

    // Emit structured event
    emit!(EmergencyWithdraw {
        token_mint,
        to: recipient,
        amount,
        caller: ctx.accounts.caller.key(),
    });
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

/// Context for forward_call instruction.
///
/// # Security Model: CPI Caller Verification
///
/// **EVM comparison:** `msg.sender == _PROTOCOL_ADAPTER` proves the PA directly invoked
/// the function. EVM's `msg.sender` is unforgeable by the runtime.
///
/// **Solana approach:** We use instruction introspection to verify the CPI caller.
/// The instructions sysvar stores only top-level instructions. During CPI execution,
/// `load_current_index_checked` returns the index of the outer (calling) instruction.
/// We verify that instruction's program_id matches `config.protocol_adapter`.
///
/// If `forward_call` is invoked directly (not via CPI), the current instruction's
/// program_id will be the forwarder itself, causing the check to reject. This makes
/// the check unforgeable — unlike a simple account key comparison, an attacker cannot
/// pass the PA's public key as an AccountInfo to bypass verification.
#[derive(Accounts)]
pub struct ForwardCall<'info> {
    #[account(
        seeds = [b"config"],
        bump = config.bump
    )]
    pub config: Account<'info, Config>,

    /// Instructions sysvar for CPI caller verification and Ed25519 signature introspection.
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
    )]
    pub config: Account<'info, Config>,

    /// Protocol Adapter state account for checking paused status.
    /// Mirrors EVM's _checkEmergencyStopped() which queries ProtocolAdapter.isEmergencyStopped().
    /// CHECK: Verified to be the correct PA state PDA derived from config.protocol_adapter
    #[account(
        constraint = pa_state.key() == derive_pa_state_pda(&config.protocol_adapter).0 @ ErrorCode::InvalidPaState
    )]
    pub pa_state: AccountInfo<'info>,
}

#[derive(Accounts)]
pub struct SetEmergencyCaller<'info> {
    pub committee: Signer<'info>,

    #[account(
        mut,
        seeds = [b"config"],
        bump = config.bump,
        constraint = config.emergency_committee == committee.key() @ ErrorCode::UnauthorizedCaller
    )]
    pub config: Account<'info, Config>,

    /// Protocol Adapter state account for checking paused status.
    /// Mirrors EVM's _checkEmergencyStopped() which queries ProtocolAdapter.isEmergencyStopped().
    /// CHECK: Verified to be the correct PA state PDA derived from config.protocol_adapter
    #[account(
        constraint = pa_state.key() == derive_pa_state_pda(&config.protocol_adapter).0 @ ErrorCode::InvalidPaState
    )]
    pub pa_state: AccountInfo<'info>,
}
