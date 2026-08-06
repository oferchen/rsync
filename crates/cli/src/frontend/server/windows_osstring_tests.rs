//! Windows byte-fidelity tests for the `--server` argv decode path.
//!
//! Windows hands `std::env::args_os()` to the process as UTF-16, and that
//! UTF-16 is not required to be well-formed: a file or directory created
//! through the NT namespace may carry an unpaired surrogate, which has no
//! UTF-8 (and therefore no `String`) representation at all. Any hop through
//! `to_string_lossy` rewrites such a unit to U+FFFD, and U+FFFD is a
//! perfectly legal filename character - so the corruption is silent and the
//! server acts on a *different* path than the client named.
//!
//! Upstream never has this failure mode: `parse_arguments(int *argc_p, const
//! char ***argv_p)` (options.c:1361) hands popt the raw `char **` argv,
//! `poptGetArgs` (options.c:2096) returns those same pointers, and
//! `poptDupArgv` copies them bytewise. Path-bearing option values are taken
//! the same way - `basis_dir[basis_dir_cnt++] = (char *)poptGetOptArg(pc)`
//! (options.c:1757) for the alt-dest flags. No stage validates or transcodes
//! the encoding, so upstream is byte-transparent end to end. These tests pin
//! the same transparency on the Windows side of oc's decode path.

use std::ffi::OsString;
use std::os::windows::ffi::{OsStrExt, OsStringExt};

use super::flags::parse_server_long_flags;
use super::parse::parse_server_flag_string_and_args;

/// A lone high surrogate: valid UTF-16 storage, but not a valid code point,
/// so it has no UTF-8 encoding and cannot survive a `String` round trip.
const LONE_SURROGATE: u16 = 0xD800;

/// Builds an `OsString` whose UTF-16 contains an unpaired surrogate between
/// `prefix` and `suffix`, mirroring how a Windows path with an ill-formed
/// name reaches `args_os()`.
fn ill_formed_os_string(prefix: &str, suffix: &str) -> OsString {
    let units: Vec<u16> = prefix
        .encode_utf16()
        .chain(std::iter::once(LONE_SURROGATE))
        .chain(suffix.encode_utf16())
        .collect();
    OsString::from_wide(&units)
}

/// Returns the raw UTF-16 units, the only representation in which an
/// unpaired surrogate is still observable.
fn wide_units(value: &OsString) -> Vec<u16> {
    value.encode_wide().collect()
}

/// The compact flag string a real `--server` invocation carries, used so the
/// parsers reach their post-flag-string state exactly as in production.
const COMPACT_FLAGS: &str = "-logDtpre.iLsfxCIvu";

/// Every long flag `server_options()` emits as two argv slots. Their value
/// slot must be consumed regardless of its encoding.
///
/// upstream: options.c:2807-2808 (`--backup-dir`), 2926-2927 (`--temp-dir`),
/// 2939-2940 (alt-dest via `safe_arg("", basis_dir[i])`), 2964-2965
/// (`--files-from`), 2886-2890 (`--partial-dir`).
const TWO_ARG_FLAGS: [&str; 7] = [
    "--compare-dest",
    "--copy-dest",
    "--link-dest",
    "--backup-dir",
    "--temp-dir",
    "--files-from",
    "--partial-dir",
];

/// A destination operand that cannot round-trip through `String` must reach
/// `ServerConfig` with its UTF-16 intact.
///
/// This is the whole point of keeping `positional_args` an `OsString` vector:
/// the operand names the directory the server will create files under. A
/// U+FFFD substitution here does not fail - it succeeds against the wrong
/// path, so the transfer reports success while the client's destination is
/// left untouched. Upstream cannot diverge this way because `poptGetArgs`
/// (options.c:2096) yields the untouched `char *` argv.
#[test]
fn positional_operand_preserves_unpaired_surrogate() {
    let dest = ill_formed_os_string("dest", "dir");
    let args = vec![
        OsString::from(COMPACT_FLAGS),
        OsString::from("."),
        dest.clone(),
    ];

    let (flag_string, positional_args) = parse_server_flag_string_and_args(&args);

    assert_eq!(flag_string, COMPACT_FLAGS);
    assert_eq!(positional_args.len(), 1);
    assert_eq!(
        wide_units(&positional_args[0]),
        wide_units(&dest),
        "the destination operand must reach the server verbatim, not via a lossy String hop"
    );
}

/// Both operands of a push (`. src dest`) keep their own ill-formed units,
/// and neither borrows the other's.
///
/// A per-argument `to_string_lossy` collapses distinct ill-formed names onto
/// the same U+FFFD spelling, which would make two different source entries
/// alias one destination. Asserting both operands separately is what makes
/// that collapse detectable.
#[test]
fn multiple_positional_operands_keep_distinct_ill_formed_names() {
    let source = ill_formed_os_string("src", "a");
    let dest = ill_formed_os_string("dest", "b");
    let args = vec![
        OsString::from(COMPACT_FLAGS),
        OsString::from("."),
        source.clone(),
        dest.clone(),
    ];

    let (_, positional_args) = parse_server_flag_string_and_args(&args);

    assert_eq!(positional_args.len(), 2);
    assert_eq!(wide_units(&positional_args[0]), wide_units(&source));
    assert_eq!(wide_units(&positional_args[1]), wide_units(&dest));
    assert_ne!(
        wide_units(&positional_args[0]),
        wide_units(&positional_args[1]),
        "distinct ill-formed names must not collapse onto a shared replacement spelling"
    );
}

/// `--partial-dir DIR` arrives as two argv slots; the value must land in
/// `ServerLongFlags::partial_dir` with its UTF-16 intact.
///
/// The partial directory is where an interrupted transfer's data is parked
/// and where the retry looks for it. A corrupted spelling silently orphans
/// the partial file: the retry finds nothing and re-sends the whole file,
/// defeating the flag. upstream: options.c:2886-2890 emits the value via
/// `safe_arg`, and the server binds it as an untouched `char *`.
#[test]
fn two_arg_partial_dir_value_preserves_unpaired_surrogate() {
    let partial_dir = ill_formed_os_string("part", "dir");
    let args = vec![
        OsString::from(COMPACT_FLAGS),
        OsString::from("--partial-dir"),
        partial_dir.clone(),
        OsString::from("."),
        OsString::from("dest/"),
    ];

    let flags = parse_server_long_flags(&args);

    let stored = flags
        .partial_dir
        .as_ref()
        .expect("--partial-dir value must be captured");
    assert_eq!(
        wide_units(stored),
        wide_units(&partial_dir),
        "the partial directory must reach the receiver verbatim"
    );
}

/// A two-arg long flag consumes its value slot even when that value is
/// ill-formed UTF-16, so the value never surfaces as a positional operand.
///
/// This is the arity invariant, not a formatting nicety. `--link-dest ALT .
/// dest/` with a leaked value slot puts `ALT` at `positional_args[0]` and
/// `dest/` at `[1]`, so the receiver treats the alt-dest basis as the
/// destination root - it mkdir's a directory that already exists and writes
/// nothing where the client asked. Encoding must not be what decides arity:
/// upstream's popt binds `POPT_ARG_STRING` values by table position
/// (options.c `long_options[]`), never by inspecting the value's bytes.
#[test]
fn two_arg_flag_consumes_ill_formed_value_slot() {
    for flag in TWO_ARG_FLAGS {
        let value = ill_formed_os_string("alt", "basis");
        let args = vec![
            OsString::from(COMPACT_FLAGS),
            OsString::from(flag),
            value,
            OsString::from("."),
            OsString::from("dest/"),
        ];

        let (_, positional_args) = parse_server_flag_string_and_args(&args);

        assert_eq!(
            positional_args,
            vec![OsString::from("dest/")],
            "{flag} must consume its ill-formed value slot instead of leaking it as an operand"
        );
    }
}

/// An ill-formed operand is not mistaken for an unknown long flag, and an
/// ill-formed value slot does not desynchronise the long-flag scan.
///
/// `parse_server_long_flags` and `parse_server_flag_string_and_args` walk the
/// same argv independently; `run_server_mode` merges their results. If only
/// one of them mishandled an ill-formed token the two walks would disagree
/// about where the operands start. Running both over the same argv is what
/// pins them together.
#[test]
fn long_flag_scan_and_operand_scan_agree_on_ill_formed_argv() {
    let value = ill_formed_os_string("alt", "basis");
    let dest = ill_formed_os_string("dest", "dir");
    let args = vec![
        OsString::from(COMPACT_FLAGS),
        OsString::from("--partial-dir"),
        value.clone(),
        OsString::from("."),
        dest.clone(),
    ];

    let flags = parse_server_long_flags(&args);
    let (_, positional_args) = parse_server_flag_string_and_args(&args);

    assert_eq!(
        wide_units(
            flags
                .partial_dir
                .as_ref()
                .expect("--partial-dir value must be captured")
        ),
        wide_units(&value)
    );
    assert_eq!(positional_args.len(), 1);
    assert_eq!(wide_units(&positional_args[0]), wide_units(&dest));
}
