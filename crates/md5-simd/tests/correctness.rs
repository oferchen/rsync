//! Correctness tests for md5-simd public API.

use md5_simd::{digest, digest_batch};

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[test]
fn single_digest_matches_rfc1321() {
    assert_eq!(to_hex(&digest(b"")), "d41d8cd98f00b204e9800998ecf8427e");
    assert_eq!(to_hex(&digest(b"abc")), "900150983cd24fb0d6963f7d28e17f72");
}

#[test]
fn batch_digest_matches_sequential() {
    let inputs: Vec<Vec<u8>> = (0..16)
        .map(|i| format!("test input {i}").into_bytes())
        .collect();

    let batch_results = digest_batch(&inputs);
    let sequential_results: Vec<_> = inputs.iter().map(|i| digest(i)).collect();

    assert_eq!(batch_results, sequential_results);
}

#[test]
fn batch_empty_returns_empty() {
    let empty: &[&[u8]] = &[];
    assert!(digest_batch(empty).is_empty());
}

#[test]
fn batch_single_matches_digest() {
    let input = b"single input";
    let batch = digest_batch(&[input]);
    assert_eq!(batch.len(), 1);
    assert_eq!(batch[0], digest(input));
}

#[test]
fn batch_with_different_lengths() {
    let inputs: &[&[u8]] = &[
        b"",
        b"a",
        b"short",
        b"a medium length string for testing",
        &[0u8; 1000],
    ];

    let batch = digest_batch(inputs);
    for (i, input) in inputs.iter().enumerate() {
        assert_eq!(batch[i], digest(input), "Mismatch at index {i}");
    }
}
