use crate::upgrade::executable_hash;
use anchor_lang::prelude::*;
use anchor_lang::solana_program::bpf_loader_upgradeable::UpgradeableLoaderState;

fn buffer(state: UpgradeableLoaderState, code: &[u8]) -> Vec<u8> {
    let mut bytes = bincode::serialize(&state).unwrap();
    bytes.extend_from_slice(code);
    bytes
}

fn holding(code: &[u8]) -> Vec<u8> {
    let bytes = buffer(
        UpgradeableLoaderState::Buffer {
            authority_address: Some(Pubkey::new_unique()),
        },
        code,
    );
    assert_eq!(
        bytes.len() - code.len(),
        UpgradeableLoaderState::size_of_buffer_metadata()
    );
    bytes
}

fn sha256(bytes: &[u8]) -> [u8; 32] {
    solana_sha256_hasher::hashv(&[bytes]).to_bytes()
}

/// `solana-verify` hashes the code with its trailing zero bytes removed, so
/// a buffer allocated larger than its code hashes as the code alone.
#[test]
fn executable_hash_is_the_sha256_of_the_code_without_trailing_zeros() {
    let code = [0x7f, b'E', b'L', b'F', 0, 0, 9, 1];
    let mut padded = code.to_vec();
    padded.extend_from_slice(&[0; 100]);
    assert_eq!(executable_hash(&holding(&code)).unwrap(), sha256(&code));
    assert_eq!(
        executable_hash(&holding(&padded)).unwrap(),
        sha256(&code),
        "trailing zero padding must not change the hash"
    );
}

#[test]
fn executable_hash_refuses_an_account_that_is_not_a_buffer() {
    let program_data = buffer(
        UpgradeableLoaderState::Program {
            programdata_address: Pubkey::new_unique(),
        },
        &[1, 2, 3],
    );
    assert!(
        executable_hash(&program_data).is_err(),
        "a Program account is not a buffer"
    );
    assert!(
        executable_hash(&[1, 0, 0]).is_err(),
        "shorter than the buffer metadata"
    );
}

#[test]
fn executable_hash_refuses_a_buffer_without_code() {
    assert!(executable_hash(&holding(&[])).is_err(), "an empty buffer");
    assert!(
        executable_hash(&holding(&[0; 64])).is_err(),
        "a buffer of zeros"
    );
}
