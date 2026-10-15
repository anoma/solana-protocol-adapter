//! The adapter repository's test forwarder (`programs/test-forwarder`), which
//! a local environment deploys with `deploy_test_forwarder`: the inputs a
//! settlement's external call gives it, and the instructions a test sends it
//! directly.

use anoma_pa_solana_client::anchor_instruction_disc;
use solana_instruction::Instruction;
use surfpool_sdk::Pubkey;

/// The `forward_call` mode that relays the call to the program its first
/// remaining account names, handing it the rest: a program the adapter
/// invokes acting as the adapter toward another forwarder.
pub const MODE_RELAY: u8 = 0x02;
/// The `forward_call` mode that logs `input[1]` lines of 100 bytes.
pub const MODE_LOG: u8 = 0x03;
/// The `forward_call` mode that writes the rest of its input to the start of
/// its first account, which the test forwarder owns, and returns it.
pub const MODE_WRITE: u8 = 0x04;
/// What a relay returns once the relayed call succeeded.
pub const RELAY_OK: u8 = 0x2a;

/// The `forward_call` input of a relay: the target's `forward_call` with the
/// logic ref `logic_ref` and the input `input`.
pub fn relay_input(logic_ref: [u8; 32], input: &[u8]) -> Vec<u8> {
    [&[MODE_RELAY], logic_ref.as_slice(), input].concat()
}

/// The `forward_call` input that writes `bytes` to the call's first account.
pub fn write_input(bytes: &[u8]) -> Vec<u8> {
    [&[MODE_WRITE], bytes].concat()
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_relay_input_is_the_mode_then_the_logic_ref_then_the_relayed_input() {
        let input = relay_input([7; 32], &[1, 2, 3]);
        assert_eq!(input[0], MODE_RELAY);
        assert_eq!(&input[1..33], &[7; 32]);
        assert_eq!(&input[33..], &[1, 2, 3]);
    }

    #[test]
    fn a_write_input_is_the_mode_then_the_bytes() {
        assert_eq!(write_input(&[1, 2, 3]), [MODE_WRITE, 1, 2, 3]);
    }
}
