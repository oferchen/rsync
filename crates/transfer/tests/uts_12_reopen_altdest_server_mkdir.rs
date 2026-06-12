//! UTS-12.REOPEN-V2 - regression coverage for the `--server` receiver +
//! split-form `--copy-dest <path>` alt-dest pre-flight mkdir chain.
//!
//! The first round of the dest-root pre-flight fix (UTS-2.d.3 / UTS-2.d.4)
//! wired `ensure_dest_root_exists` into every `setup_transfer` dispatch arm,
//! but the alt-dest interop test running over `lsh.sh` continued to fail
//! with `FileNotFoundError: /tmp/alt-dest/to`. The root cause was upstream
//! of the helper: upstream `options.c::server_options()` emits `--copy-dest`
//! (and its `--link-dest` / `--compare-dest` siblings) as TWO adjacent argv
//! slots via `safe_arg("", basis_dir[i])`. The server's `--server` argument
//! parser only recognised the `--copy-dest=VALUE` joined form, so the bare
//! `--copy-dest` was treated as unknown and the basis path landed in
//! `positional_args[0]`, displacing the actual destination root.
//!
//! `setup_transfer` reads `config.args.first()` as the dest operand, so
//! the helper fired against the already-existing alt-dest basis (returning
//! `Ok(false)`) and the real destination root `/tmp/alt-dest/to/` was
//! never created. The matching parser fix is pinned by unit tests in
//! `crates/cli/src/frontend/server/tests.rs`; this integration test pins
//! the receiver-side half of the contract by driving
//! [`transfer::receiver::ensure_dest_root_exists`] with the resolved
//! destination path and asserting the missing root materialises.
//!
//! Splitting the regression coverage across both crates avoids the
//! integration test reaching into `pub(super)` parser surface while still
//! producing a single failure signal if either layer regresses (the
//! parser change makes the upstream argv shape yield the correct dest
//! path; this test proves the helper acts on that dest).
//!
//! ## Upstream Reference
//!
//! - `main.c:778-792 get_local_name()` - pre-flight `do_mkdir(dest_path, ACCESSPERMS)`
//!   when `file_total > 1 || trailing_slash`.
//! - `options.c:2939-2940` - `alt_dest_opt(0)` + `safe_arg("", basis_dir[i])`
//!   emits `--copy-dest`, `--link-dest`, `--compare-dest` as two argv slots.

use std::fs;

use tempfile::tempdir;

use transfer::receiver::ensure_dest_root_exists;

/// UTS-12.REOPEN-V2: the receiver-side pre-flight must materialise the
/// destination root reported by the parser when the alt-dest argv shape
/// (`-a --copy-dest /alt . /dest/`) arrives over remote shell.
///
/// Replays the exact destination path the upstream alt-dest interop
/// test passes (multi-file transfer, trailing-slash dest, non-dry-run)
/// against an alt-dest basis seeded by the operator. Once the parser
/// resolves the post-`.` positional as the dest (pinned by the cli
/// unit tests), the helper must create the missing root before the
/// per-entry mkdir dispatch runs.
// upstream: main.c:778-792 get_local_name() pre-flight, exercised by
// the upstream alt-dest test over lsh.sh.
#[test]
fn alt_dest_server_chain_creates_missing_dest_root() {
    let tmp = tempdir().expect("tempdir");
    let alt_basis = tmp.path().join("alt3");
    let dest_root = tmp.path().join("missing/dest/root");

    // Operator-seeded alt-dest basis: upstream's main.c:798-806 only
    // checks that the basis path resolves. The pre-flight owns the
    // destination root, NOT the basis.
    fs::create_dir_all(&alt_basis).expect("seed alt-dest basis");
    fs::write(alt_basis.join("seed.txt"), b"basis").expect("seed basis file");

    assert!(
        !dest_root.exists(),
        "destination root must start missing to prove the pre-flight ran"
    );

    // The argv shape upstream sends over `lsh.sh`:
    //     --server -vlogDtpre.iLsfxCIvu --copy-dest /tmp/alt3 . /tmp/to/
    // After the cli parser fix (pinned by
    // `parse_server_args_handles_split_copy_dest_form` in
    // `crates/cli/src/frontend/server/tests.rs`), the `--copy-dest /alt3`
    // pair is drained out of the positional list and the trailing
    // `/tmp/to/` becomes `config.args[0]`. `setup_transfer` then reads
    // that path as `dest_dir` and calls `ensure_dest_root_exists` with
    // it. The mkdir must materialise the dest.
    let file_total = 5; // multi-file alt-dest source tree
    let trailing_slash = true; // upstream argv carries `/tmp/to/`
    let dry_run = false;

    let created = ensure_dest_root_exists(&dest_root, file_total, trailing_slash, dry_run)
        .expect("alt-dest --copy-dest pre-flight must auto-create the dest");

    assert!(
        created,
        "the helper must report the missing dest root as newly created \
         under the --server + split-form --copy-dest chain"
    );
    assert!(
        dest_root.is_dir(),
        "the dest root (multiple directory components deep) must exist \
         as a directory after the pre-flight (create_dir_all semantics)"
    );
    assert!(
        alt_basis.join("seed.txt").exists(),
        "the alt-dest basis must remain intact - the pre-flight only \
         owns the destination root"
    );
}

/// UTS-12.REOPEN-V2 + EDG-DESTROOT.1: the helper must NOT mkdir when the
/// transfer is exactly one non-directory entry without a trailing-slash
/// dest. This is the upstream `main.c:805-832 get_local_name()` rename
/// branch, where the lone payload is written to the operand path under
/// `change_dir(parent)`. The pre-flight returning `Ok(false)` here is
/// the contract: `setup_transfer::apply_single_file_rename` then rewrites
/// the lone flist entry's name to the operand basename.
// upstream: main.c:805-832 single-file rename branch
#[test]
fn single_file_without_trailing_slash_does_not_mkdir_destroot() {
    let tmp = tempdir().expect("tempdir");
    let dest_root = tmp.path().join("would-be-file");

    let created =
        ensure_dest_root_exists(&dest_root, 1, false, false).expect("helper must succeed");

    assert!(
        !created,
        "single non-directory transfer without trailing slash must not \
         mkdir the dest root; upstream takes the rename branch instead"
    );
    assert!(
        !dest_root.exists(),
        "dest root must remain untouched in the single-file rename branch"
    );
}

/// UTS-12.REOPEN-V2 + EDG-DESTROOT.4: the helper rejects a destination
/// that resolves to a regular file with a typed error. Mirrors upstream
/// `main.c:756-761`: when `file_total > 1` and `S_ISDIR` fails, upstream
/// emits "destination must be a directory" and exits with
/// `RERR_FILESELECT`. The Rust port produces an `io::Error::InvalidInput`
/// with the same semantic.
// upstream: main.c:756-761
#[test]
fn dest_existing_as_file_returns_typed_error() {
    let tmp = tempdir().expect("tempdir");
    let dest_file = tmp.path().join("not-a-directory");
    fs::write(&dest_file, b"existing file").expect("seed dest as a file");

    let err = ensure_dest_root_exists(&dest_file, 3, true, false)
        .expect_err("dest existing as a regular file must be rejected");

    assert_eq!(
        err.kind(),
        std::io::ErrorKind::InvalidInput,
        "regular-file dest must surface as InvalidInput (got {:?})",
        err.kind()
    );
}
