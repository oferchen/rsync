//! Non-UTF-8 byte fidelity through compile + match + covers (CVE-2022-29154).
//!
//! Upstream rsync's matcher operates on raw C byte strings: `exclude.c:1002`
//! `rule_matches()` compares `const char *` names with `strcmp`/`wildmatch`,
//! and `lib/wildmatch.c:64` `dowild()` walks `const uchar *`. Two distinct
//! non-UTF-8 byte sequences therefore never alias. A lossy UTF-8 conversion
//! on either side folds every invalid sequence to U+FFFD, so distinct byte
//! names collide - the receiver then accepts a file-list name the client
//! never requested (a false-accept on the CVE-2022-29154 implied-include
//! check) or rejects a name it did request.
#![cfg(unix)]

use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use filters::{FilterRule, FilterSet, ImpliedIncludeOptions, ImpliedIncludes};

fn bytes_path(bytes: &[u8]) -> &Path {
    Path::new(OsStr::from_bytes(bytes))
}

/// The task-224 driving defect at the unit boundary: the client requested the
/// latin-1 name `caf\xe9`. Core's operand plumbing historically lossy-decoded
/// it to `caf\u{FFFD}` before the implied-include rules were built. A
/// malicious sender then injects the DIFFERENT name `caf\x80`: upstream's
/// byte matcher rejects it (`\x80` != `\xe9`, flist.c:1369 "rejecting
/// unrequested file-list name"), but a lossy match input also folds `\x80`
/// to U+FFFD and falsely accepts the injected name.
#[test]
fn lossy_alias_of_requested_name_is_not_covered() {
    let opts = ImpliedIncludeOptions {
        recurse: true,
        ..Default::default()
    };
    // Byte-faithful arg: what the client actually typed.
    let implied = ImpliedIncludes::from_args(opts, [b"caf\xe9".as_slice()]).unwrap();

    // The requested name itself is covered.
    assert!(implied.covers(bytes_path(b"caf\xe9"), true));
    assert!(implied.covers(bytes_path(b"caf\xe9/inner"), false));

    // An injected name whose lossy rendering collides with the requested one
    // must stay rejected (upstream: exclude.c:1002 rule_matches byte compare).
    assert!(!implied.covers(bytes_path(b"caf\x80"), true));
    assert!(!implied.covers(bytes_path(b"caf\x80/inner"), false));
    // The U+FFFD spelling itself was never requested either.
    assert!(!implied.covers(Path::new("caf\u{FFFD}"), true));
}

/// A pattern that legitimately contains U+FFFD (a valid UTF-8 name) must not
/// admit raw invalid-byte names that merely render as U+FFFD.
#[test]
fn genuine_replacement_char_request_rejects_raw_byte_aliases() {
    let opts = ImpliedIncludeOptions {
        recurse: true,
        ..Default::default()
    };
    let implied = ImpliedIncludes::from_args(opts, ["caf\u{FFFD}"]).unwrap();

    assert!(implied.covers(Path::new("caf\u{FFFD}"), true));
    // Distinct raw bytes; upstream strcmp says no.
    assert!(!implied.covers(bytes_path(b"caf\x80"), true));
    assert!(!implied.covers(bytes_path(b"caf\xe9"), true));
}

/// Wildcard args match single NON-UTF-8 bytes exactly like upstream
/// `dowild()`: `?` consumes one byte, `*` any run of bytes.
#[test]
fn wildcard_args_match_raw_bytes_like_dowild() {
    let opts = ImpliedIncludeOptions {
        recurse: true,
        ..Default::default()
    };
    let implied = ImpliedIncludes::from_args(opts, [b"caf?".as_slice()]).unwrap();

    // upstream lib/wildmatch.c:64 dowild() - `?` matches any single byte
    // except `/`, including an invalid-UTF-8 one.
    assert!(implied.covers(bytes_path(b"caf\xe9"), true));
    assert!(implied.covers(bytes_path(b"caf\x80"), true));
    assert!(implied.covers(Path::new("cafe"), true));
    // Two bytes do not satisfy a single `?`.
    assert!(!implied.covers(bytes_path(b"caf\xc3\xa9"), true));
}

/// Exclude rules built from raw bytes match byte-identically on the
/// receiver's re-check surface (`allows_during_traversal`), and non-UTF-8
/// names never alias UTF-8 patterns.
#[test]
fn exclude_rules_round_trip_non_utf8_compile_match() {
    let set = FilterSet::from_rules([FilterRule::exclude(b"secret\xff*".to_vec())]).unwrap();

    assert!(!set.allows_during_traversal(bytes_path(b"secret\xff-plans"), false));
    // A different invalid byte is a different name: allowed.
    assert!(set.allows_during_traversal(bytes_path(b"secret\xfe-plans"), false));
    // The lossy rendering of the pattern must not be excluded.
    assert!(set.allows_during_traversal(Path::new("secret\u{FFFD}-plans"), false));
}
