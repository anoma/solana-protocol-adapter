//! Merkle tree constants, hash function, and state operations for the commitment tree.
//!
//! The hash function and padding leaf must match arm-risc0's implementation exactly.

use anchor_lang::prelude::*;
use arm_core::Digest;
use solana_sha256_hasher::hashv;

use crate::error::PAError;
use crate::state::PAStateAccount;

/// Initial tree depth for new commitment trees (capacity = 2 leaves). pa-evm
/// starts at depth 0 (capacity 1); both grow by doubling.
pub const INITIAL_TREE_DEPTH: usize = 1;

/// Maximum tree depth for the commitment tree.
/// Supports up to 2^32 = 4,294,967,296 leaves.
pub const MAX_TREE_DEPTH: usize = 32;

/// Precomputed zero hashes at each level of the tree.
/// zeros[0] = PADDING_LEAF, zeros[i] = hash(zeros[i-1], zeros[i-1]).
/// Precomputed to avoid ~200k CU cost of on-chain hash computation.
/// All 32 levels are needed even for variable-depth trees (for root computation).
pub const ZEROS: [Digest; MAX_TREE_DEPTH] = [
    // Level 0: cc1d2f838445db7aec431df9ee8a871f40e7aa5e064fc056633ef8c60fab7b06
    Digest::new([
        0x832f1dcc, 0x7adb4584, 0xf91d43ec, 0x1f878aee, 0x5eaae740, 0x56c04f06, 0xc6f83e63,
        0x067bab0f,
    ]),
    // Level 1: 9052a1e9c2367d7248294f9ebc9495a73e2ff0c1b5e1dd09531e0a08ac1edc13
    Digest::new([
        0xe9a15290, 0x727d36c2, 0x9e4f2948, 0xa79594bc, 0xc1f02f3e, 0x09dde1b5, 0x080a1e53,
        0x13dc1eac,
    ]),
    // Level 2: 78da8fe62dc0b46ba21971b6ab1b873c071f16a5f3f102216b8660884c3712e9
    Digest::new([
        0xe68fda78, 0x6bb4c02d, 0xb67119a2, 0x3c871bab, 0xa5161f07, 0x2102f1f3, 0x8860866b,
        0xe912374c,
    ]),
    // Level 3: e86b1f2d8ec1204d36481a46d3afc0fca0cc77386505ce4060cfa2eae6ba9547
    Digest::new([
        0x2d1f6be8, 0x4d20c18e, 0x461a4836, 0xfcc0afd3, 0x3877cca0, 0x40ce0565, 0xeaa2cf60,
        0x4795bae6,
    ]),
    // Level 4: 45c2f1b8d817e7dbb818ce089b402f5eb2e981dbade474cadce703f356dc3d67
    Digest::new([
        0xb8f1c245, 0xdbe717d8, 0x08ce18b8, 0x5e2f409b, 0xdb81e9b2, 0xca74e4ad, 0xf303e7dc,
        0x673ddc56,
    ]),
    // Level 5: 1b8dffac32c2afa124fef2dcdc231dc0a601bbc14f2392672412d295908f5399
    Digest::new([
        0xacff8d1b, 0xa1afc232, 0xdcf2fe24, 0xc01d23dc, 0xc1bb01a6, 0x6792234f, 0x95d21224,
        0x99538f90,
    ]),
    // Level 6: f4987c63cdab12aafee277155a5d06f67bf721027acd9106b3af895128616899
    Digest::new([
        0x637c98f4, 0xaa12abcd, 0x1577e2fe, 0xf6065d5a, 0x0221f77b, 0x0691cd7a, 0x5189afb3,
        0x99686128,
    ]),
    // Level 7: 760486b25ee7b732959e88c7cb305a03f426052953ddff48f8ff3dbeba209f9f
    Digest::new([
        0xb2860476, 0x32b7e75e, 0xc7889e95, 0x035a30cb, 0x290526f4, 0x48ffdd53, 0xbe3dfff8,
        0x9f9f20ba,
    ]),
    // Level 8: 9fd2b7153c9aac55a4220e24a6734810cf3ddd38da3607f29ac4c42df5802b56
    Digest::new([
        0x15b7d29f, 0x55ac9a3c, 0x240e22a4, 0x104873a6, 0x38dd3dcf, 0xf20736da, 0x2dc4c49a,
        0x562b80f5,
    ]),
    // Level 9: 18db5dfe794336af2963876d47dc97738b45276ecb326e53431bbf734cab79f0
    Digest::new([
        0xfe5ddb18, 0xaf364379, 0x6d876329, 0x7397dc47, 0x6e27458b, 0x536e32cb, 0x73bf1b43,
        0xf079ab4c,
    ]),
    // Level 10: d337ccd9a386fe657d315b4561dd126731960ec391ea97730fea2b8bdad775ba
    Digest::new([
        0xd9cc37d3, 0x65fe86a3, 0x455b317d, 0x6712dd61, 0xc30e9631, 0x7397ea91, 0x8b2bea0f,
        0xba75d7da,
    ]),
    // Level 11: 1be19a4cd40ebc4f1eaacf0a746bebcc0ebd41cf4e6c5efac87c9366e36ec5d4
    Digest::new([
        0x4c9ae11b, 0x4fbc0ed4, 0x0acfaa1e, 0xcceb6b74, 0xcf41bd0e, 0xfa5e6c4e, 0x66937cc8,
        0xd4c56ee3,
    ]),
    // Level 12: febc00e0f250cd3a1c33e60dd4b620f5661d831f65ed4364a5eeb46753adf661
    Digest::new([
        0xe000bcfe, 0x3acd50f2, 0x0de6331c, 0xf520b6d4, 0x1f831d66, 0x6443ed65, 0x67b4eea5,
        0x61f6ad53,
    ]),
    // Level 13: aa1519e851f0c734a289c57f36f301f4ed10c46f3403c7360b5d896afe3c2eba
    Digest::new([
        0xe81915aa, 0x34c7f051, 0x7fc589a2, 0xf401f336, 0x6fc410ed, 0x36c70334, 0x6a895d0b,
        0xba2e3cfe,
    ]),
    // Level 14: 53692a35b69e0384ae8c1a78fc7324e109ea45a98f8c463dd40563d3fe92b939
    Digest::new([
        0x352a6953, 0x84039eb6, 0x781a8cae, 0xe12473fc, 0xa945ea09, 0x3d468c8f, 0xd36305d4,
        0x39b992fe,
    ]),
    // Level 15: b449a011fdee39a0fa67cd2041bead15b224b1ab7665811a8881d6d2aaadabd4
    Digest::new([
        0x11a049b4, 0xa039eefd, 0x20cd67fa, 0x15adbe41, 0xabb124b2, 0x1a816576, 0xd2d68188,
        0xd4abadaa,
    ]),
    // Level 16: af5bf3704beccfde7534a4d61dfc55262c60f542b30c11781165e0664422d0ab
    Digest::new([
        0x70f35baf, 0xdecfec4b, 0xd6a43475, 0x2655fc1d, 0x42f5602c, 0x78110cb3, 0x66e06511,
        0xabd02244,
    ]),
    // Level 17: d6be3b998bc09d7fac4f4a3e0f1751349cd4ab0c0d798e66c50c42d5d31346f7
    Digest::new([
        0x993bbed6, 0x7f9dc08b, 0x3e4a4fac, 0x3451170f, 0x0cabd49c, 0x668e790d, 0xd5420cc5,
        0xf74613d3,
    ]),
    // Level 18: 071105ef6f4550af999d417a5ce3e1d4e7534e3fb4737437e17006dde0dcee02
    Digest::new([
        0xef051107, 0xaf50456f, 0x7a419d99, 0xd4e1e35c, 0x3f4e53e7, 0x377473b4, 0xdd0670e1,
        0x02eedce0,
    ]),
    // Level 19: 0901a3f66292e7c70d4970a2dfa58334b8e96f8044c3daf41ae4420acef258c3
    Digest::new([
        0xf6a30109, 0xc7e79262, 0xa270490d, 0x3483a5df, 0x806fe9b8, 0xf4dac344, 0x0a42e41a,
        0xc358f2ce,
    ]),
    // Level 20: f6f60db4f294f6373397178312dbc00d837f0edaaa1eb9247664927121228029
    Digest::new([
        0xb40df6f6, 0x37f694f2, 0x83179733, 0x0dc0db12, 0xda0e7f83, 0x24b91eaa, 0x71926476,
        0x29802221,
    ]),
    // Level 21: a5b6b9b7a4de7bb26a9faa11037b5989d016f88e3dd9555d0e59972a292e97cb
    Digest::new([
        0xb7b9b6a5, 0xb27bdea4, 0x11aa9f6a, 0x89597b03, 0x8ef816d0, 0x5d55d93d, 0x2a97590e,
        0xcb972e29,
    ]),
    // Level 22: c4096b4abf3e7ba7a29322611be1e6fcfecaba653222e634e15bd6822109a6f2
    Digest::new([
        0x4a6b09c4, 0xa77b3ebf, 0x612293a2, 0xfce6e11b, 0x65bacafe, 0x34e62232, 0x82d65be1,
        0xf2a60921,
    ]),
    // Level 23: ad941c01e5acd3963536227309af0e40a6e27e0c2d6d5ecfd76da708169d1ff3
    Digest::new([
        0x011c94ad, 0x96d3ace5, 0x73223635, 0x400eaf09, 0x0c7ee2a6, 0xcf5e6d2d, 0x08a76dd7,
        0xf31f9d16,
    ]),
    // Level 24: 223f70c38dda1063f13b23fd5fca5085f58705d82d59566e4ce92256c36c4cd5
    Digest::new([
        0xc3703f22, 0x6310da8d, 0xfd233bf1, 0x8550ca5f, 0xd80587f5, 0x6e56592d, 0x5622e94c,
        0xd54c6cc3,
    ]),
    // Level 25: 1e717c1005b410c7db7d9de50f4161626db27769bcd2370f5e0e271d84cae27f
    Digest::new([
        0x107c711e, 0xc710b405, 0xe59d7ddb, 0x6261410f, 0x6977b26d, 0x0f37d2bc, 0x1d270e5e,
        0x7fe2ca84,
    ]),
    // Level 26: 7af85d77d28dffe5d1181f573d0fa40cfc87416eebbfe9a02c75ca3402ecfbfe
    Digest::new([
        0x775df87a, 0xe5ff8dd2, 0x571f18d1, 0x0ca40f3d, 0x6e4187fc, 0xa0e9bfeb, 0x34ca752c,
        0xfefbec02,
    ]),
    // Level 27: 765b92a62ae38f274085c9b3e098bcbeb83dcb3618099f1b968c6eeefb85738c
    Digest::new([
        0xa6925b76, 0x278fe32a, 0xb3c98540, 0xbebc98e0, 0x36cb3db8, 0x1b9f0918, 0xee6e8c96,
        0x8c7385fb,
    ]),
    // Level 28: 9a16ba8601e0fcc863c4dd5766c1fa445452cb582f6091ec72b238863f888511
    Digest::new([
        0x86ba169a, 0xc8fce001, 0x57ddc463, 0x44fac166, 0x58cb5254, 0xec91602f, 0x8638b272,
        0x1185883f,
    ]),
    // Level 29: 21cdfe66a766b1faff0bcfec88776a7629629deccc40cb3aa3c5da147b93093a
    Digest::new([
        0x66fecd21, 0xfab166a7, 0xeccf0bff, 0x766a7788, 0xec9d6229, 0x3acb40cc, 0x14dac5a3,
        0x3a09937b,
    ]),
    // Level 30: bea1e263505974f7bd1be386b6a6d677867830425ddc45a71b5a413b129dbae1
    Digest::new([
        0x63e2a1be, 0xf7745950, 0x86e31bbd, 0x77d6a6b6, 0x42307886, 0xa745dc5d, 0x3b415a1b,
        0xe1ba9d12,
    ]),
    // Level 31: 254f102fd2a0b5db3926704ac4f559a767f60854fc157b2de5d5853da9b8976a
    Digest::new([
        0x2f104f25, 0xdbb5a0d2, 0x4a702639, 0xa759f5c4, 0x5408f667, 0x2d7b15fc, 0x3d85d5e5,
        0x6a97b8a9,
    ]),
];

/// Empty tree root at initial depth (depth 1) = ZEROS[0] = PADDING_LEAF.
pub const EMPTY_TREE_ROOT_INITIAL: Digest = ZEROS[INITIAL_TREE_DEPTH - 1];

/// Hash two digests together (SHA-256).
/// Must match arm-risc0's hash_two implementation.
/// Concatenates the byte representations and hashes with SHA-256.
/// Uses Solana's sol_sha256 syscall for efficiency (~100 CU vs ~6k CU for sha2 crate).
/// See test_sha256_syscall_matches_sha2_crate for equivalence verification.
pub fn hash_two(left: &Digest, right: &Digest) -> Digest {
    let result = hashv(&[left.as_bytes(), right.as_bytes()]);
    Digest::from_bytes(result.to_bytes())
}

/// Append a single commitment to the tree.
///
/// Direct port of the EVM reference implementation (MerkleTree.sol `push`):
/// 1. Walk the branch at the current depth, hashing up from the leaf.
///    - Left child: store in frontier, hash with zero sibling.
///    - Right child: hash with frontier (left sibling).
/// 2. After the walk, if the tree is now full, expand by one level:
///    store the computed hash as the new frontier entry and hash it
///    with `ZEROS[depth]` to produce the root at the new depth.
///
/// Below `MAX_TREE_DEPTH`, this means a tree with exactly 2^d leaves has
/// depth d+1 and its root includes one level of zero-padding — matching the
/// EVM PA. At `MAX_TREE_DEPTH` the tree cannot expand further (`can_grow`
/// gates step 2), so the root stays the plain depth-32 root instead.
pub fn append_to_tree(state: &mut PAStateAccount, leaf: Digest) -> Result<()> {
    // The tree is full only at maximum depth with every slot used: below the
    // maximum, filling a level grows immediately and doubles capacity, so
    // next_index < capacity always holds afterwards. Reject before any mutation.
    require!(
        state.next_index < state.capacity(),
        PAError::TreeMaxDepthReached
    );

    let depth = state.depth();
    let mut index = state.next_index;
    state.next_index += 1;

    let mut current = leaf;
    for (level, zero) in ZEROS.iter().enumerate().take(depth) {
        if index & 1 == 0 {
            // Left child — store in frontier, hash with zero sibling.
            state.set_frontier(level, current);
            current = hash_two(&current, zero);
        } else {
            // Right child — hash with frontier (left sibling).
            current = hash_two(&state.get_frontier(level), &current);
        }
        index >>= 1;
    }

    // Expand only when there is a further leaf to place. At maximum depth the
    // tree stays at depth 32 and its root is the plain depth-32 root.
    if state.next_index == state.capacity() && state.can_grow() {
        let new_level = state.grow();
        state.set_frontier(new_level, current);
        current = hash_two(&current, &ZEROS[new_level]);
    }

    state.root = current.into();
    Ok(())
}

/// Required tree depth after `final_next_index` leaves have been appended.
///
/// Matches the EVM MerkleTree.sol expand-after-fill semantics: a tree with
/// exactly 2^d leaves has depth d+1 (the expansion happened when the last
/// slot was filled). For non-power-of-two counts, depth = ceil(log2(N)).
///
/// Equivalently: depth = bit-length of `final_next_index`.
pub fn required_depth_for_leaves(final_next_index: u64) -> usize {
    if final_next_index == 0 {
        return INITIAL_TREE_DEPTH;
    }
    let bits = 64 - final_next_index.leading_zeros();
    (bits as usize).max(INITIAL_TREE_DEPTH)
}
