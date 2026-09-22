//! Property test: `filters::wildmatch` agrees with a naive reference matcher
//! on random RAW BYTE patterns and texts, including non-UTF-8 sequences.
//!
//! Upstream `lib/wildmatch.c:64` `dowild()` walks `const uchar *` - every
//! byte, valid UTF-8 or not, is one match unit. The reference below encodes
//! that byte-at-a-time semantic directly (backtracking on `*`/`**`), so any
//! divergence flags a byte-fidelity bug in the production matcher.

use filters::wildmatch;
use proptest::prelude::*;

/// Naive reference for the pattern subset the generator below emits:
/// literal bytes, `?` (one byte, not `/`), `*` (any run without `/`) and
/// `**` (any run including `/`). upstream: lib/wildmatch.c:76-119.
fn reference_match(pattern: &[u8], text: &[u8]) -> bool {
    match pattern.first() {
        None => text.is_empty(),
        Some(b'?') => match text.first() {
            Some(&b) if b != b'/' => reference_match(&pattern[1..], &text[1..]),
            _ => false,
        },
        Some(b'*') => {
            // Collapse the `*` run; two or more stars cross `/`.
            let mut stars = 1;
            while pattern.get(stars) == Some(&b'*') {
                stars += 1;
            }
            let rest = &pattern[stars..];
            let cross_slash = stars >= 2;
            for split in 0..=text.len() {
                if reference_match(rest, &text[split..]) {
                    return true;
                }
                if let Some(&b) = text.get(split) {
                    if b == b'/' && !cross_slash {
                        break;
                    }
                }
            }
            false
        }
        Some(&p) => match text.first() {
            Some(&b) if b == p => reference_match(&pattern[1..], &text[1..]),
            _ => false,
        },
    }
}

/// Bytes drawn from an alphabet that mixes ASCII, `/`, wildcards, and
/// invalid-UTF-8 bytes (`0x80`, `0xE9`, `0xFF`).
fn pattern_byte() -> impl Strategy<Value = u8> {
    prop::sample::select(vec![b'a', b'b', b'/', b'.', b'?', b'*', 0x80, 0xE9, 0xFF])
}

fn text_byte() -> impl Strategy<Value = u8> {
    prop::sample::select(vec![b'a', b'b', b'/', b'.', 0x80, 0xE9, 0xFF])
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2048))]

    /// oc's byte wildmatch and the reference agree byte-for-byte.
    #[test]
    fn wildmatch_agrees_with_byte_reference(
        pattern in prop::collection::vec(pattern_byte(), 0..10),
        text in prop::collection::vec(text_byte(), 0..16),
    ) {
        prop_assert_eq!(
            wildmatch(&pattern, &text),
            reference_match(&pattern, &text),
            "pattern={:?} text={:?}",
            pattern,
            text
        );
    }

    /// A non-UTF-8 byte only ever matches itself (or a wildcard) - never a
    /// different invalid byte and never the U+FFFD replacement sequence.
    #[test]
    fn invalid_bytes_never_alias(
        prefix in prop::collection::vec(prop::sample::select(vec![b'a', b'b']), 0..6),
    ) {
        let mut pat = prefix.clone();
        pat.push(0xE9);
        let mut same = prefix.clone();
        same.push(0xE9);
        let mut other = prefix.clone();
        other.push(0x80);
        let mut fffd = prefix;
        fffd.extend_from_slice("\u{FFFD}".as_bytes());

        prop_assert!(wildmatch(&pat, &same));
        prop_assert!(!wildmatch(&pat, &other));
        prop_assert!(!wildmatch(&pat, &fffd));
    }
}
