//! Integration coverage for `--reflink=<MODE>`.
//!
//! Verifies that the tri-state CLI flag parses to the matching
//! [`fast_io::CowPolicy`] variant, that the binary `--cow`/`--no-cow`
//! form remains observable through the same field, and that an invalid
//! value is rejected at parse time so misuse is caught before any I/O
//! happens.

use cli::test_utils::parse_args;

/// `--reflink=auto` parses cleanly and is wire-equivalent to the default
/// when no other reflink-related flag is on the command line.
#[test]
fn reflink_auto_parses() {
    let args = parse_args(["oc-rsync", "--reflink=auto", "src", "dst"]).expect("parse");
    assert_eq!(args.cow_policy, fast_io::CowPolicy::Auto);

    let default = parse_args(["oc-rsync", "src", "dst"]).expect("parse default");
    assert_eq!(default.cow_policy, args.cow_policy);
}

/// `--reflink=always` parses to `CowPolicy::Required`, the
/// no-silent-fallback variant.
#[test]
fn reflink_always_parses_to_required() {
    let args = parse_args(["oc-rsync", "--reflink=always", "src", "dst"]).expect("parse");
    assert_eq!(args.cow_policy, fast_io::CowPolicy::Required);
}

/// `--reflink=never` parses to `CowPolicy::Disabled`, equivalent to the
/// existing `--no-cow` binary form.
#[test]
fn reflink_never_parses_to_disabled() {
    let args = parse_args(["oc-rsync", "--reflink=never", "src", "dst"]).expect("parse");
    assert_eq!(args.cow_policy, fast_io::CowPolicy::Disabled);

    let from_binary =
        parse_args(["oc-rsync", "--no-cow", "src", "dst"]).expect("parse binary form");
    assert_eq!(args.cow_policy, from_binary.cow_policy);
}

/// Any non-tri-state value must fail at parse time with a message that
/// names the flag and lists the valid values. This catches typos before
/// any filesystem work and matches the same error shape used by
/// `--simd=<LEVEL>`.
#[test]
fn reflink_bogus_value_rejected() {
    let err = parse_args(["oc-rsync", "--reflink=sometimes", "src", "dst"])
        .expect_err("bogus value must error");
    let rendered = err.to_string();
    assert!(
        rendered.contains("--reflink"),
        "error must name the flag: {rendered}"
    );
    assert!(
        rendered.contains("auto") && rendered.contains("always") && rendered.contains("never"),
        "error must list valid values: {rendered}"
    );
}

/// When both `--reflink` and the binary `--cow`/`--no-cow` form appear,
/// the one later on the command line wins. This mirrors upstream rsync's
/// left-to-right option processing and matches the existing
/// `--cow`/`--no-cow` precedence.
#[test]
fn last_occurrence_wins_between_reflink_and_binary_form() {
    let reflink_last =
        parse_args(["oc-rsync", "--no-cow", "--reflink=always", "src", "dst"]).expect("parse");
    assert_eq!(reflink_last.cow_policy, fast_io::CowPolicy::Required);

    let binary_last =
        parse_args(["oc-rsync", "--reflink=always", "--no-cow", "src", "dst"]).expect("parse");
    assert_eq!(binary_last.cow_policy, fast_io::CowPolicy::Disabled);
}
