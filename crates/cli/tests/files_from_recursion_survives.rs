//! `--files-from` must not discard an explicitly requested recursion.
//!
//! upstream: options.c:2188-2191 - `if (files_from) { if (recurse == 1)
//! recurse = 0; ... }`. Only the value `-a` implies (`recurse == 1`,
//! options.c:1546) is cleared; `-r` sets `recurse == 2` (options.c:621) and
//! survives, and `--old-dirs` re-forces `recurse = 1` at options.c:2197-2199,
//! after the files-from clearing has run.
//!
//! Clearing recursion unconditionally made every directory named in a
//! `--files-from` list transfer as nothing at all: against rsync 3.4.4 the same
//! invocation copied the directory's whole subtree while oc-rsync copied zero
//! files. `recursive_override` carries the distinction, so these tests pin it.

use cli::test_utils::parse_args;

#[test]
fn explicit_r_is_marked_as_surviving_files_from() {
    let args = parse_args([
        "oc-rsync",
        "-r",
        "--files-from=/tmp/list.txt",
        "src/",
        "dst/",
    ])
    .expect("parses");
    assert!(args.recursive, "-r sets recursion");
    assert_eq!(
        args.recursive_override,
        Some(true),
        "an explicit -r must survive --files-from (upstream recurse == 2)"
    );
}

#[test]
fn archive_implied_recursion_is_not_marked_as_surviving() {
    let args = parse_args([
        "oc-rsync",
        "-a",
        "--files-from=/tmp/list.txt",
        "src/",
        "dst/",
    ])
    .expect("parses");
    assert!(args.recursive, "-a implies recursion");
    assert_eq!(
        args.recursive_override, None,
        "recursion implied by -a is cleared by --files-from (upstream recurse == 1)"
    );
}

#[test]
fn old_dirs_recursion_survives_files_from() {
    // upstream: options.c:2197-2199 - `xfer_dirs >= 4` re-forces `recurse = 1`
    // after the files-from block, so --old-dirs recursion outlives the list.
    let args = parse_args([
        "oc-rsync",
        "--old-dirs",
        "--files-from=/tmp/list.txt",
        "src/",
        "dst/",
    ])
    .expect("parses");
    assert!(args.recursive, "--old-dirs forces recursion");
    assert_eq!(
        args.recursive_override,
        Some(true),
        "--old-dirs recursion must survive --files-from"
    );
}

#[test]
fn no_recursive_is_not_marked_as_surviving() {
    let args = parse_args([
        "oc-rsync",
        "-a",
        "--no-recursive",
        "--files-from=/tmp/list.txt",
        "src/",
        "dst/",
    ])
    .expect("parses");
    assert!(!args.recursive, "--no-recursive wins over -a");
    assert_eq!(
        args.recursive_override,
        Some(false),
        "--no-recursive is an explicit negative, not a surviving recursion"
    );
}
