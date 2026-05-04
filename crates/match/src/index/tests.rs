use super::*;
use protocol::ProtocolVersion;
use signature::{SignatureLayoutParams, calculate_signature_layout, generate_file_signature};
use std::collections::VecDeque;
use std::num::NonZeroU8;

#[test]
fn from_signature_returns_none_without_full_blocks() {
    let params = SignatureLayoutParams::new(
        64,
        None,
        ProtocolVersion::NEWEST,
        NonZeroU8::new(16).unwrap(),
    );
    let layout = calculate_signature_layout(params).expect("layout");
    let data = vec![0u8; 64];
    let signature = generate_file_signature(data.as_slice(), layout, SignatureAlgorithm::Md4)
        .expect("signature");

    assert!(DeltaSignatureIndex::from_signature(&signature, SignatureAlgorithm::Md4).is_none());
}

#[test]
fn find_match_bytes_locates_full_block() {
    let data = vec![b'a'; 1500];
    let params = SignatureLayoutParams::new(
        data.len() as u64,
        None,
        ProtocolVersion::NEWEST,
        NonZeroU8::new(16).unwrap(),
    );
    let layout = calculate_signature_layout(params).expect("layout");
    let signature = generate_file_signature(data.as_slice(), layout, SignatureAlgorithm::Md4)
        .expect("signature");
    let index =
        DeltaSignatureIndex::from_signature(&signature, SignatureAlgorithm::Md4).expect("index");

    let digest = index.block(0).rolling();
    let window = vec![b'a'; index.block_length()];
    let found = index.find_match_bytes(digest, &window).expect("match");
    assert_eq!(found, 0);
}

#[test]
fn find_match_window_handles_split_buffers() {
    let data = vec![b'a'; 2048];
    let params = SignatureLayoutParams::new(
        data.len() as u64,
        None,
        ProtocolVersion::NEWEST,
        NonZeroU8::new(16).unwrap(),
    );
    let layout = calculate_signature_layout(params).expect("layout");
    let signature = generate_file_signature(data.as_slice(), layout, SignatureAlgorithm::Md4)
        .expect("signature");
    let index =
        DeltaSignatureIndex::from_signature(&signature, SignatureAlgorithm::Md4).expect("index");

    let digest = index.block(0).rolling();
    let mut window = VecDeque::with_capacity(index.block_length());
    let mut scratch = Vec::with_capacity(index.block_length());
    for &byte in &data[..index.block_length()] {
        window.push_back(byte);
    }
    // Rotate the deque to force a split backing store.
    for _ in 0..5 {
        let byte = window.pop_front().unwrap();
        window.push_back(byte);
    }

    let found = index
        .find_match_window(digest, &window, &mut scratch)
        .expect("match");
    assert_eq!(found, 0);
}

#[test]
fn delta_signature_index_block_length() {
    let data = vec![b'a'; 2048];
    let params = SignatureLayoutParams::new(
        data.len() as u64,
        None,
        ProtocolVersion::NEWEST,
        NonZeroU8::new(16).unwrap(),
    );
    let layout = calculate_signature_layout(params).expect("layout");
    let signature = generate_file_signature(data.as_slice(), layout, SignatureAlgorithm::Md4)
        .expect("signature");
    let index =
        DeltaSignatureIndex::from_signature(&signature, SignatureAlgorithm::Md4).expect("index");

    assert!(index.block_length() > 0);
}

#[test]
fn delta_signature_index_strong_length() {
    let data = vec![b'a'; 2048];
    let params = SignatureLayoutParams::new(
        data.len() as u64,
        None,
        ProtocolVersion::NEWEST,
        NonZeroU8::new(16).unwrap(),
    );
    let layout = calculate_signature_layout(params).expect("layout");
    let signature = generate_file_signature(data.as_slice(), layout, SignatureAlgorithm::Md4)
        .expect("signature");
    let index =
        DeltaSignatureIndex::from_signature(&signature, SignatureAlgorithm::Md4).expect("index");

    assert_eq!(index.strong_length(), 16);
}

#[test]
fn delta_signature_index_block_accessor() {
    let data = vec![b'a'; 2048];
    let params = SignatureLayoutParams::new(
        data.len() as u64,
        None,
        ProtocolVersion::NEWEST,
        NonZeroU8::new(16).unwrap(),
    );
    let layout = calculate_signature_layout(params).expect("layout");
    let signature = generate_file_signature(data.as_slice(), layout, SignatureAlgorithm::Md4)
        .expect("signature");
    let index =
        DeltaSignatureIndex::from_signature(&signature, SignatureAlgorithm::Md4).expect("index");

    let block = index.block(0);
    assert_eq!(block.len(), index.block_length());
}

#[test]
fn delta_signature_index_clone() {
    let data = vec![b'a'; 2048];
    let params = SignatureLayoutParams::new(
        data.len() as u64,
        None,
        ProtocolVersion::NEWEST,
        NonZeroU8::new(16).unwrap(),
    );
    let layout = calculate_signature_layout(params).expect("layout");
    let signature = generate_file_signature(data.as_slice(), layout, SignatureAlgorithm::Md4)
        .expect("signature");
    let index =
        DeltaSignatureIndex::from_signature(&signature, SignatureAlgorithm::Md4).expect("index");
    let cloned = index.clone();

    assert_eq!(index.block_length(), cloned.block_length());
    assert_eq!(index.strong_length(), cloned.strong_length());
}

#[test]
fn delta_signature_index_debug() {
    let data = vec![b'a'; 2048];
    let params = SignatureLayoutParams::new(
        data.len() as u64,
        None,
        ProtocolVersion::NEWEST,
        NonZeroU8::new(16).unwrap(),
    );
    let layout = calculate_signature_layout(params).expect("layout");
    let signature = generate_file_signature(data.as_slice(), layout, SignatureAlgorithm::Md4)
        .expect("signature");
    let index =
        DeltaSignatureIndex::from_signature(&signature, SignatureAlgorithm::Md4).expect("index");

    let debug = format!("{index:?}");
    assert!(debug.contains("DeltaSignatureIndex"));
}

#[test]
fn find_match_bytes_wrong_length_returns_none() {
    let data = vec![b'a'; 2048];
    let params = SignatureLayoutParams::new(
        data.len() as u64,
        None,
        ProtocolVersion::NEWEST,
        NonZeroU8::new(16).unwrap(),
    );
    let layout = calculate_signature_layout(params).expect("layout");
    let signature = generate_file_signature(data.as_slice(), layout, SignatureAlgorithm::Md4)
        .expect("signature");
    let index =
        DeltaSignatureIndex::from_signature(&signature, SignatureAlgorithm::Md4).expect("index");

    let digest = index.block(0).rolling();
    let window = vec![b'a'; index.block_length() - 1];
    assert!(index.find_match_bytes(digest, &window).is_none());
}

#[test]
fn find_match_bytes_no_match_returns_none() {
    let data = vec![b'a'; 2048];
    let params = SignatureLayoutParams::new(
        data.len() as u64,
        None,
        ProtocolVersion::NEWEST,
        NonZeroU8::new(16).unwrap(),
    );
    let layout = calculate_signature_layout(params).expect("layout");
    let signature = generate_file_signature(data.as_slice(), layout, SignatureAlgorithm::Md4)
        .expect("signature");
    let index =
        DeltaSignatureIndex::from_signature(&signature, SignatureAlgorithm::Md4).expect("index");

    let digest = index.block(0).rolling();
    let window = vec![b'z'; index.block_length()];
    assert!(index.find_match_bytes(digest, &window).is_none());
}

#[test]
fn find_match_window_wrong_length_returns_none() {
    let data = vec![b'a'; 2048];
    let params = SignatureLayoutParams::new(
        data.len() as u64,
        None,
        ProtocolVersion::NEWEST,
        NonZeroU8::new(16).unwrap(),
    );
    let layout = calculate_signature_layout(params).expect("layout");
    let signature = generate_file_signature(data.as_slice(), layout, SignatureAlgorithm::Md4)
        .expect("signature");
    let index =
        DeltaSignatureIndex::from_signature(&signature, SignatureAlgorithm::Md4).expect("index");

    let digest = index.block(0).rolling();
    let mut window = VecDeque::new();
    let mut scratch = Vec::new();
    for _ in 0..index.block_length() - 1 {
        window.push_back(b'a');
    }
    assert!(
        index
            .find_match_window(digest, &window, &mut scratch)
            .is_none()
    );
}

/// Tests that rolling checksum collisions are resolved by strong checksum.
///
/// Creates a file with multiple blocks that could have rolling checksum
/// collisions in the hash table, verifying the strong checksum disambiguates.
#[test]
fn find_match_bytes_uses_strong_checksum_for_collision() {
    // Block 0 and block 2 share the same content (and therefore the same
    // rolling checksum); block 1 carries a distinct pattern. The lookup must
    // still resolve each block correctly via the strong checksum.
    let block_size = 700usize;
    let mut data = vec![0u8; block_size * 3];

    for (i, byte) in data[..block_size].iter_mut().enumerate() {
        *byte = (i % 256) as u8;
    }
    for (i, byte) in data[block_size..2 * block_size].iter_mut().enumerate() {
        *byte = ((i + 128) % 256) as u8;
    }
    for (i, byte) in data[2 * block_size..].iter_mut().enumerate() {
        *byte = (i % 256) as u8;
    }

    let params = SignatureLayoutParams::new(
        data.len() as u64,
        None,
        ProtocolVersion::NEWEST,
        NonZeroU8::new(16).unwrap(),
    );
    let layout = calculate_signature_layout(params).expect("layout");
    let signature = generate_file_signature(data.as_slice(), layout, SignatureAlgorithm::Md4)
        .expect("signature");
    let index =
        DeltaSignatureIndex::from_signature(&signature, SignatureAlgorithm::Md4).expect("index");

    let block0_digest = index.block(0).rolling();
    let block0_window: Vec<u8> = data[..index.block_length()].to_vec();
    let found0 = index.find_match_bytes(block0_digest, &block0_window);
    assert!(found0.is_some(), "block 0 should match");

    let block1_digest = index.block(1).rolling();
    let block1_start = index.block_length();
    let block1_window: Vec<u8> = data[block1_start..block1_start + index.block_length()].to_vec();
    let found1 = index.find_match_bytes(block1_digest, &block1_window);
    assert!(found1.is_some(), "block 1 should match");

    // Same rolling-checksum lookup key, but different content: the strong
    // checksum disambiguates and rejects the false positive.
    let no_match = index.find_match_bytes(block0_digest, &block1_window);
    assert!(
        no_match.is_none(),
        "wrong content should not match despite same digest key"
    );
}

#[test]
fn rebuild_reuses_allocation() {
    let data1 = vec![b'a'; 2048];
    let params1 = SignatureLayoutParams::new(
        data1.len() as u64,
        None,
        ProtocolVersion::NEWEST,
        NonZeroU8::new(16).unwrap(),
    );
    let layout1 = calculate_signature_layout(params1).expect("layout1");
    let sig1 =
        generate_file_signature(data1.as_slice(), layout1, SignatureAlgorithm::Md4).expect("sig1");
    let mut index =
        DeltaSignatureIndex::from_signature(&sig1, SignatureAlgorithm::Md4).expect("index");

    let capacity_before = index.lookup.capacity();

    let data2 = vec![b'b'; 3000];
    let params2 = SignatureLayoutParams::new(
        data2.len() as u64,
        None,
        ProtocolVersion::NEWEST,
        NonZeroU8::new(16).unwrap(),
    );
    let layout2 = calculate_signature_layout(params2).expect("layout2");
    let sig2 =
        generate_file_signature(data2.as_slice(), layout2, SignatureAlgorithm::Md4).expect("sig2");

    let has_full = index.rebuild(&sig2, SignatureAlgorithm::Md4);
    assert!(has_full, "second signature should have full blocks");

    // The lookup-table allocation must be reused, not shrunk, after rebuild.
    assert!(
        index.lookup.capacity() >= capacity_before,
        "capacity should be preserved across rebuild"
    );

    let digest = index.block(0).rolling();
    let window = vec![b'b'; index.block_length()];
    let found = index.find_match_bytes(digest, &window);
    assert!(found.is_some(), "should find a match after rebuild");

    let old_window = vec![b'a'; index.block_length()];
    let old_found = index.find_match_bytes(digest, &old_window);
    assert!(
        old_found.is_none(),
        "old data should not match after rebuild"
    );
}

/// `find_match_slices` with a single contiguous slice (empty second) matches
/// `find_match_bytes`.
#[test]
fn find_match_slices_contiguous_matches_find_match_bytes() {
    let data = vec![b'a'; 1500];
    let params = SignatureLayoutParams::new(
        data.len() as u64,
        None,
        ProtocolVersion::NEWEST,
        NonZeroU8::new(16).unwrap(),
    );
    let layout = calculate_signature_layout(params).expect("layout");
    let signature = generate_file_signature(data.as_slice(), layout, SignatureAlgorithm::Md4)
        .expect("signature");
    let index =
        DeltaSignatureIndex::from_signature(&signature, SignatureAlgorithm::Md4).expect("index");

    let digest = index.block(0).rolling();
    let window = vec![b'a'; index.block_length()];
    let found_bytes = index.find_match_bytes(digest, &window);
    let found_slices = index.find_match_slices(digest, &window, &[]);
    assert_eq!(found_bytes, found_slices);
}

/// `find_match_slices` with a split window finds the same block as `find_match_bytes`.
#[test]
fn find_match_slices_split_window_finds_block() {
    let data = vec![b'a'; 1500];
    let params = SignatureLayoutParams::new(
        data.len() as u64,
        None,
        ProtocolVersion::NEWEST,
        NonZeroU8::new(16).unwrap(),
    );
    let layout = calculate_signature_layout(params).expect("layout");
    let signature = generate_file_signature(data.as_slice(), layout, SignatureAlgorithm::Md4)
        .expect("signature");
    let index =
        DeltaSignatureIndex::from_signature(&signature, SignatureAlgorithm::Md4).expect("index");

    let digest = index.block(0).rolling();
    let window = vec![b'a'; index.block_length()];
    let split_at = index.block_length() / 3;
    let (first, second) = window.split_at(split_at);
    let found = index.find_match_slices(digest, first, second);
    assert!(found.is_some(), "split window should find a match");
    assert_eq!(found, index.find_match_bytes(digest, &window));
}

/// `find_match_slices` rejects windows with wrong combined length.
#[test]
fn find_match_slices_wrong_length_returns_none() {
    let data = vec![b'a'; 2048];
    let params = SignatureLayoutParams::new(
        data.len() as u64,
        None,
        ProtocolVersion::NEWEST,
        NonZeroU8::new(16).unwrap(),
    );
    let layout = calculate_signature_layout(params).expect("layout");
    let signature = generate_file_signature(data.as_slice(), layout, SignatureAlgorithm::Md4)
        .expect("signature");
    let index =
        DeltaSignatureIndex::from_signature(&signature, SignatureAlgorithm::Md4).expect("index");

    let digest = index.block(0).rolling();
    let short_window = vec![b'a'; index.block_length() - 1];
    assert!(
        index
            .find_match_slices(digest, &short_window, &[])
            .is_none()
    );
}
