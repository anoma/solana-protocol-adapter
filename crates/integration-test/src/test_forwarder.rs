//! The adapter repository's test forwarder (`programs/test-forwarder`), which
//! a local environment deploys with `deploy_test_forwarder`, and the
//! instructions a test sends it directly.

use anoma_pa_solana_client::anchor_instruction_disc;
use solana_instruction::Instruction;
use surfpool_sdk::Pubkey;

/// The `forward_call` mode that logs `input[1]` lines of 100 bytes.
pub const MODE_LOG: u8 = 0x03;

/// The test forwarder `program`'s `forward_call` in log mode, as an
/// instruction of its own: it logs `lines` lines of 100 bytes, which fill a
/// transaction's program-log budget for the instructions after it.
pub fn log_ix(program: &Pubkey, lines: u8) -> Instruction {
    let input = [MODE_LOG, lines];
    let mut data = anchor_instruction_disc("forward_call").to_vec();
    data.extend_from_slice(&[0; 32]);
    data.extend_from_slice(&(input.len() as u32).to_le_bytes());
    data.extend_from_slice(&input);
    Instruction::new_with_bytes(*program, &data, vec![])
}
