//! A sender operand whose last component is `..` is a DOTDIR operand.
//!
//! This is the wire half of the same rule the local-copy executor carries.
//! Upstream appends `/.` to such an operand and marks it `DOTDIR_NAME`
//! (`flist.c:2595-2602`), so the non-relative split at `flist.c:2610-2621` cuts
//! `src/../.` at its LAST `/`: `dir` is `src/..`, `fn` is `.`, and the parent
//! directory becomes the walk root whose CONTENTS ride the wire under their own
//! names.
//!
//! Without that, oc handed `Path::parent()` the job: `src/..` got a walk base of
//! `src`, and the purely lexical `strip_prefix` left the transmitted name `..`.
//! This half was the SILENT one - no error is raised at the sender. MEASURED
//! against rsync 3.5.0 over an `--rsh` loopback: oc-rsync died at the RECEIVER
//! with "ABORTING due to unsafe pathname from sender: .." (exit 4) where
//! upstream exits 0 having transferred the parent's contents.
//!
//! Assertions are on the transmitted NAMES, since that is where the defect
//! lives; the receiver's refusal is a downstream symptom of them.
//!
//! # Scope: NOT under `--relative`
//!
//! `flist.c:2581-2583` short-circuits the marker chain to `NORMAL_NAME` under
//! `--relative`, and `flist.c:2658-2667` rejects a `..` in the active part of a
//! relative operand instead. `parent_dir_rule_is_scoped_to_non_relative` pins
//! that boundary.
//!
//! # Upstream Reference
//!
//! - `rsync-3.5.0/flist.c:2595-2602` - the `/.` append and `DOTDIR_NAME`.
//! - `rsync-3.5.0/flist.c:2581-2583` - the `--relative` short-circuit above it.
//! - `rsync-3.5.0/flist.c:2610-2621` - the non-relative last-`/` split.
//! - `rsync-3.5.0/flist.c:2658-2667` - the `--relative` `..` rejection.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use tempfile::TempDir;

use protocol::ProtocolVersion;
use protocol::flist::FileType;
use transfer::{
    GeneratorContext, HandshakeResult, ServerConfig, ServerRole, TransferPhase, TransferPipeline,
};

/// A non-relative recursive sender. `-R` is deliberately absent: the rule under
/// test lives in the arm `--relative` short-circuits past.
fn generator_config(operand: &Path, relative: bool) -> ServerConfig {
    let flags = if relative {
        "-rR.iLsfxCIvu"
    } else {
        "-r.iLsfxCIvu"
    };
    let mut config = ServerConfig::from_flag_string_and_args(
        ServerRole::Generator,
        flags.to_owned(),
        vec![operand.to_path_buf().into_os_string()],
    )
    .expect("server config");
    config.connection.client_mode = true;
    assert_eq!(
        config.flags.relative, relative,
        "the --relative scope of this cell must be the one it asked for"
    );
    assert!(
        !config.flags.copy_dirlinks,
        "the walk must descend because of the operand marker, not because of -k"
    );
    assert!(!config.flags.copy_links, "-L would follow every symlink");
    config
}

fn test_handshake() -> HandshakeResult {
    HandshakeResult {
        protocol: ProtocolVersion::try_from(32u8).unwrap(),
        buffered: Vec::new(),
        compat_exchanged: true,
        client_args: None,
        io_timeout: None,
        negotiated_algorithms: None,
        compat_flags: None,
        checksum_seed: 0,
    }
}

fn test_pipeline() -> TransferPipeline {
    let mut pipeline = TransferPipeline::new(ServerRole::Generator);
    pipeline
        .advance_to(TransferPhase::FilterExchange)
        .expect("advance to FilterExchange");
    pipeline
        .advance_to(TransferPhase::FileListTransfer)
        .expect("advance to FileListTransfer");
    pipeline
}

/// Plants, under `<scratch>`:
///
/// ```text
/// realdir/{f.txt, sub/g.txt}
/// symdir -> realdir
/// ```
///
/// so `realdir/..` is `<scratch>` and its contents are `realdir` plus `symdir`.
fn build_source_tree(scratch: &TempDir) {
    let realdir = scratch.path().join("realdir");
    fs::create_dir_all(realdir.join("sub")).expect("mkdir realdir/sub");
    fs::write(realdir.join("f.txt"), b"payload").expect("write realdir/f.txt");
    fs::write(realdir.join("sub/g.txt"), b"deeper").expect("write realdir/sub/g.txt");
    symlink("realdir", scratch.path().join("symdir")).expect("plant symdir -> realdir");
}

/// Builds the flist for `operand` and returns every transmitted name paired
/// with its file type, in wire order.
fn flist(operand: &Path, relative: bool) -> Vec<(String, FileType)> {
    let config = generator_config(operand, relative);
    let mut ctx = GeneratorContext::new(&test_handshake(), config, test_pipeline());
    ctx.build_file_list(&[operand.to_path_buf()])
        .expect("build_file_list");
    ctx.file_list()
        .iter()
        .map(|e| (e.name().to_owned(), e.file_type()))
        .collect()
}

fn names(rows: &[(String, FileType)]) -> Vec<&str> {
    rows.iter().map(|(n, _)| n.as_str()).collect()
}

/// `realdir/..` transfers the parent's CONTENTS, rooted at `.`, with no name
/// containing a `..` component.
///
/// Sorted, because `build_file_list` runs before `flist_sort_and_clean()`
/// (`flist.c:2816`) and emits in directory-walk order. The property under test
/// is WHICH names the operand selects, not the order they were discovered in.
#[test]
fn parent_dir_operand_sends_the_parents_contents_rooted_at_dot() {
    let scratch = TempDir::new().expect("tempdir");
    build_source_tree(&scratch);
    let operand = scratch.path().join("realdir/..");

    let rows = flist(&operand, false);
    let mut got = names(&rows);
    got.sort_unstable();

    assert_eq!(
        got,
        vec![
            ".",
            "realdir",
            "realdir/f.txt",
            "realdir/sub",
            "realdir/sub/g.txt",
            "symdir",
        ],
        "`<dir>/..` is DOTDIR_NAME (flist.c:2595-2602): the parent becomes the \
         transfer root `.` and its contents ride under their own names",
    );
}

/// Not one transmitted name may carry a `..` component. This is the byte the
/// receiver refuses ("ABORTING due to unsafe pathname from sender: ..").
#[test]
fn no_transmitted_name_carries_a_parent_dir_component() {
    let scratch = TempDir::new().expect("tempdir");
    build_source_tree(&scratch);

    for suffix in ["/..", "/../", "/../.", "/./.."] {
        let operand = PathBuf::from(format!("{}/realdir{suffix}", scratch.path().display()));
        let rows = flist(&operand, false);
        let unsafe_names: Vec<&str> = names(&rows)
            .into_iter()
            .filter(|n| {
                *n == ".." || n.starts_with("../") || n.ends_with("/..") || n.contains("/../")
            })
            .collect();
        assert!(
            unsafe_names.is_empty(),
            "`realdir{suffix}` put {unsafe_names:?} on the wire; every conforming \
             receiver refuses those: flist.c:851-855 aborts with RERR_UNSUPPORTED (4) when clean_fname(CFN_REFUSE_DOT_DOT_DIRS) rejects the name",
        );
    }
}

/// The `..` spelling is indistinguishable on the wire from the DOTDIR spellings
/// oc already handled.
#[test]
fn parent_dir_operand_matches_the_trailing_slash_and_dot_spellings() {
    let mut seen: Vec<(&str, Vec<String>)> = Vec::new();
    for suffix in ["/..", "/../", "/../.", "/./.."] {
        let scratch = TempDir::new().expect("tempdir");
        build_source_tree(&scratch);
        let operand = PathBuf::from(format!("{}/realdir{suffix}", scratch.path().display()));
        let rows = flist(&operand, false);
        let mut got: Vec<String> = names(&rows).iter().map(|s| (*s).to_owned()).collect();
        got.sort_unstable();
        seen.push((suffix, got));
    }

    let (first_suffix, first) = &seen[0];
    for (suffix, got) in &seen[1..] {
        assert_eq!(
            got, first,
            "`realdir{suffix}` and `realdir{first_suffix}` are both DOTDIR \
             operands (flist.c:2584-2604) and must transmit identical names",
        );
    }
}

/// NEGATIVE CONTROL. An ordinary directory operand is `NORMAL_NAME`
/// (`flist.c:2605-2606`) and its names are rooted at its own basename. Nothing
/// here touches the trailing-`..` rule, so this cell must stay green whether
/// that rule is present or reverted.
#[test]
fn an_ordinary_directory_operand_still_sends_itself() {
    let scratch = TempDir::new().expect("tempdir");
    build_source_tree(&scratch);

    let rows = flist(&scratch.path().join("realdir"), false);

    assert_eq!(
        names(&rows),
        vec![
            "realdir",
            "realdir/f.txt",
            "realdir/sub",
            "realdir/sub/g.txt"
        ],
        "a NORMAL_NAME operand is rooted at its own basename, not at `.`",
    );
}

/// A `..` that is not the LAST component is `NORMAL_NAME`. This is the cell a
/// fix that always appended `/.`, or that tested `contains("..")`, would fail:
/// `symdir` would arrive as a `.`-rooted content listing instead of one entry.
#[test]
fn a_dotdot_that_is_not_the_last_component_is_a_normal_name() {
    let scratch = TempDir::new().expect("tempdir");
    build_source_tree(&scratch);

    let rows = flist(&scratch.path().join("realdir/../symdir"), false);

    assert_eq!(
        rows,
        vec![("symdir".to_owned(), FileType::Symlink)],
        "upstream's guard is `fbuf[len-1] == '.' && fbuf[len-2] == '.' && \
         (len == 2 || fbuf[len-3] == '/')` (flist.c:2595-2596) - an interior \
         `..` never reaches the append, so the operand stays a single \
         NORMAL_NAME symlink entry",
    );
}

/// The rule is scoped to NON-relative operands. Under `--relative` upstream
/// forces `NORMAL_NAME` (`flist.c:2581-2583`) and then rejects the operand at
/// `flist.c:2658-2667`; it never grants it contents semantics.
///
/// oc does not implement that rejection yet (MEASURED: rsync 3.5.0 exits 1 with
/// `found ".." dir in relative path: ...`, oc does not - a KNOWN residual
/// divergence, unchanged by this fix). What this cell pins is the SCOPE: the
/// `--relative` names must not become the `.`-rooted DOTDIR listing.
#[test]
fn parent_dir_rule_is_scoped_to_non_relative() {
    let scratch = TempDir::new().expect("tempdir");
    build_source_tree(&scratch);

    let rows = flist(&scratch.path().join("realdir/.."), true);
    // Sorted on BOTH sides: an order-sensitive `assert_ne!` would pass on a
    // reordering alone and never test the names at all.
    let mut got = names(&rows);
    got.sort_unstable();

    assert_ne!(
        got,
        vec![
            ".",
            "realdir",
            "realdir/f.txt",
            "realdir/sub",
            "realdir/sub/g.txt",
            "symdir",
        ],
        "`--relative` must not acquire the non-relative arm's DOTDIR contents \
         semantics - upstream rejects the operand there instead \
         (flist.c:2658-2667)",
    );
}
