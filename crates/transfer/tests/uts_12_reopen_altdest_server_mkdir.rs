//! Regression coverage for the `--server` receiver +
//! split-form `--copy-dest <path>` alt-dest pre-flight mkdir chain.
//!
//! The first round of the dest-root pre-flight fix wired
//! `ensure_dest_root_exists` into every `setup_transfer` dispatch arm, but
//! the alt-dest interop test running over `lsh.sh` continued to fail
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
//! - `main.c:808-822 get_local_name()` - pre-flight `do_mkdir(dest_path, ACCESSPERMS)`
//!   when `file_total > 1 || trailing_slash`.
//! - `options.c:3120-3121` - `alt_dest_opt(0)` + `safe_arg("", basis_dir[i])`
//!   emits `--copy-dest`, `--link-dest`, `--compare-dest` as two argv slots.

use std::fs;

use tempfile::tempdir;

use transfer::receiver::ensure_dest_root_exists;

/// The receiver-side pre-flight must materialise the
/// destination root reported by the parser when the alt-dest argv shape
/// (`-a --copy-dest /alt . /dest/`) arrives over remote shell.
///
/// Replays the exact destination path the upstream alt-dest interop
/// test passes (multi-file transfer, trailing-slash dest, non-dry-run)
/// against an alt-dest basis seeded by the operator. Once the parser
/// resolves the post-`.` positional as the dest (pinned by the cli
/// unit tests), the helper must create the missing root before the
/// per-entry mkdir dispatch runs.
// upstream: main.c:808-822 get_local_name() pre-flight, exercised by
// the upstream alt-dest test over lsh.sh.
#[test]
fn alt_dest_server_chain_creates_missing_dest_root() {
    let tmp = tempdir().expect("tempdir");
    let alt_basis = tmp.path().join("alt3");
    // Single missing leaf whose parent already exists: upstream `main.c:817`
    // does a single `do_mkdir(dest_path)`, which creates one level. A deeper
    // chain (`missing/dest/root`) would fail without `--mkpath` (verified
    // against upstream 3.4.4: `mkdir ... failed: No such file or directory`),
    // so the pre-flight only owns the final component here.
    let dest_root = tmp.path().join("to");

    // Operator-seeded alt-dest basis: upstream's main.c:913-916
    // check_alt_basis_dirs() only warns when the basis path does not
    // resolve. The pre-flight owns the destination root, NOT the basis.
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
    let mkpath = false; // upstream forwards --mkpath only on client request

    let created = ensure_dest_root_exists(&dest_root, file_total, trailing_slash, dry_run, mkpath)
        .expect("alt-dest --copy-dest pre-flight must auto-create the dest");

    assert!(
        created,
        "the helper must report the missing dest root as newly created \
         under the --server + split-form --copy-dest chain"
    );
    assert!(
        dest_root.is_dir(),
        "the single missing dest-root level must exist as a directory \
         after the pre-flight (do_mkdir semantics)"
    );
    assert!(
        alt_basis.join("seed.txt").exists(),
        "the alt-dest basis must remain intact - the pre-flight only \
         owns the destination root"
    );
}

/// The helper must NOT mkdir when the transfer is exactly one
/// non-directory entry without a trailing-slash dest. This is the upstream
/// `main.c:845-872 get_local_name()` rename branch, where the lone payload
/// is written to the operand path under `change_dir(parent)`. The pre-flight
/// returning `Ok(false)` here is the contract:
/// `setup_transfer::apply_single_file_rename` then rewrites
/// the lone flist entry's name to the operand basename.
// upstream: main.c:845-872 single-file rename branch
#[test]
fn single_file_without_trailing_slash_does_not_mkdir_destroot() {
    let tmp = tempdir().expect("tempdir");
    let dest_root = tmp.path().join("would-be-file");

    let created =
        ensure_dest_root_exists(&dest_root, 1, false, false, false).expect("helper must succeed");

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

/// The helper rejects a destination that resolves to a regular file with
/// a typed error. Mirrors upstream
/// `main.c:786-791`: when `file_total > 1` and `S_ISDIR` fails, upstream
/// emits "destination must be a directory" and exits with
/// `RERR_FILESELECT`. The Rust port produces an `io::Error::InvalidInput`
/// with the same semantic.
// upstream: main.c:786-791
#[test]
fn dest_existing_as_file_returns_typed_error() {
    let tmp = tempdir().expect("tempdir");
    let dest_file = tmp.path().join("not-a-directory");
    fs::write(&dest_file, b"existing file").expect("seed dest as a file");

    let err = ensure_dest_root_exists(&dest_file, 3, true, false, false)
        .expect_err("dest existing as a regular file must be rejected");

    assert_eq!(
        err.kind(),
        std::io::ErrorKind::InvalidInput,
        "regular-file dest must surface as InvalidInput (got {:?})",
        err.kind()
    );
}
