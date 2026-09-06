//! A `--relative` operand's DOTDIR marker must not ride along in its NAME.
//!
//! Upstream keeps two things apart that are easy to fuse. The transmitted name
//! is `clean_fname(fn, CFN_KEEP_TRAILING_SLASH | CFN_DROP_TRAILING_DOT_DIR)`
//! with the trailing `/` then stripped (`flist.c:2642-2657`); the fact that a
//! marker was there survives only in `name_type`, which is what makes
//! `link_stat()` follow a symlinked operand at `flist.c:2697`. So `-R src/d`,
//! `-R src/d/` and `-R src/d/.` all put the SAME names on the wire, and
//! `-R sym/` differs from `-R sym` only in that the symlink is followed.
//!
//! Leaving the marker in the name is not cosmetic: `src/d/` sorts AFTER its own
//! child `src/d/f.txt` under `f_name_cmp`, so the receiver's generator sees an
//! order the sender never intended and the NDX echo desynchronises. Measured on
//! oc-rsync before this pin: `oc-rsync -aR src/d/ h:dst/` died with
//! "sender echoed NDX 4 but expected 2 - protocol violation (code 12)" and left
//! `f.txt` untransferred. The `/.` spelling emitted `src/d/./f.txt` names that
//! only landed correctly because the destination filesystem collapses them.
//!
//! ⚠ These cells use ABSOLUTE operands, so the `--relative` base is `/` and
//! `strip_prefix` against it re-slices through `Components`, which absorbs a
//! trailing `/` on its own. The trailing-slash spelling is therefore NOT
//! discriminating here - it is pinned byte-exactly by
//! `generator::file_list::relative_operand_name_tests` instead. What these cells
//! do pin is the `/.` spelling, which `Components` does not absorb, plus the
//! follow decision in both directions.
//!
//! # Upstream Reference
//!
//! - `rsync-3.5.0/flist.c:2642` - `len = clean_fname(fn,
//!   CFN_KEEP_TRAILING_SLASH | CFN_DROP_TRAILING_DOT_DIR);`
//! - `rsync-3.5.0/flist.c:2651-2657` - `else if (fn[len-1] == '/') {
//!   fn[--len] = '\0'; ... name_type = SLASH_ENDING_NAME; }`
//! - `rsync-3.5.0/flist.c:2697` - `link_stat(fbuf, &st, copy_dirlinks ||
//!   name_type != NORMAL_NAME)` - the marker's surviving effect.

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

fn generator_config(operand: &Path) -> ServerConfig {
    let mut config = ServerConfig::from_flag_string_and_args(
        ServerRole::Generator,
        "-rR.iLsfxCIvu".to_owned(),
        vec![operand.to_path_buf().into_os_string()],
    )
    .expect("server config");
    config.connection.client_mode = true;
    assert!(config.flags.relative, "-R must parse as --relative");
    assert!(
        !config.flags.copy_dirlinks,
        "the follow must come from the operand marker, not from -k"
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

/// `<scratch>/realdir/{f.txt,sub/g.txt}` plus `<scratch>/symdir -> realdir`.
fn build_source_tree(scratch: &TempDir) {
    let realdir = scratch.path().join("realdir");
    fs::create_dir_all(realdir.join("sub")).expect("mkdir realdir/sub");
    fs::write(realdir.join("f.txt"), b"payload").expect("write realdir/f.txt");
    fs::write(realdir.join("sub/g.txt"), b"deeper").expect("write realdir/sub/g.txt");
    symlink("realdir", scratch.path().join("symdir")).expect("plant symdir -> realdir");
}

/// Builds the flist for `operand` and returns the transmitted names that live
/// under `scratch`, in wire order, paired with their file types.
///
/// Names above the scratch directory are the operand's implied parents
/// (`flist.c:1937 send_implied_dirs()`); they are identical for every shape
/// under test and would only bury the rows that differ.
fn scoped_flist(scratch: &TempDir, operand: &Path) -> Vec<(String, FileType)> {
    let config = generator_config(operand);
    let mut ctx = GeneratorContext::new(&test_handshake(), config, test_pipeline());
    ctx.build_file_list(&[operand.to_path_buf()])
        .expect("build_file_list");

    // The receiver strips the leading `/` (flist.c:3071), so the sender's names
    // are the absolute path minus its root.
    let root = scratch
        .path()
        .to_str()
        .expect("scratch path is utf-8")
        .trim_start_matches('/')
        .to_owned();
    let prefix = format!("{root}/");
    ctx.file_list()
        .iter()
        .filter_map(|e| {
            e.name()
                .strip_prefix(&prefix)
                .map(|rest| (rest.to_owned(), e.file_type()))
        })
        .collect()
}

fn names_of(rows: &[(String, FileType)]) -> Vec<&str> {
    rows.iter().map(|(n, _)| n.as_str()).collect()
}

/// The three spellings of a real directory operand are indistinguishable on the
/// wire, and none of them may carry a marker in a name.
#[test]
fn relative_directory_operand_sends_the_same_names_for_every_spelling() {
    let expected = [
        "realdir",
        "realdir/f.txt",
        "realdir/sub",
        "realdir/sub/g.txt",
    ];

    let mut report = Vec::new();
    let mut failed = false;
    for suffix in ["", "/", "/."] {
        let scratch = TempDir::new().expect("tempdir");
        build_source_tree(&scratch);
        let operand = PathBuf::from(format!("{}/realdir{suffix}", scratch.path().display()));

        let rows = scoped_flist(&scratch, &operand);
        let got = names_of(&rows);
        let row_ok = got == expected;
        failed |= !row_ok;
        report.push(format!(
            "  realdir{suffix:<2} [{}] left: {got:?}\n              right: {expected:?}",
            if row_ok { "pass" } else { "FAIL" },
        ));
    }

    assert!(
        !failed,
        "`-R <dir>`, `-R <dir>/` and `-R <dir>/.` must transmit identical \
         names: upstream normalises the operand at flist.c:2642-2657 and keeps \
         the marker in `name_type`, never in the name:\n{}",
        report.join("\n"),
    );
}

/// No transmitted name may contain a `/` run, an interior `.` component, or a
/// trailing separator - the shapes `clean_fname()` exists to remove.
#[test]
fn relative_operand_names_are_clean_fname_normalised() {
    let mut report = Vec::new();
    let mut failed = false;
    for base in ["realdir", "symdir"] {
        for suffix in ["", "/", "/."] {
            let scratch = TempDir::new().expect("tempdir");
            build_source_tree(&scratch);
            let operand = PathBuf::from(format!("{}/{base}{suffix}", scratch.path().display()));

            let rows = scoped_flist(&scratch, &operand);
            let dirty: Vec<&str> = names_of(&rows)
                .into_iter()
                .filter(|n| {
                    n.contains("//") || n.contains("/./") || n.ends_with('/') || n.ends_with("/.")
                })
                .collect();
            failed |= !dirty.is_empty();
            report.push(format!(
                "  {base}{suffix:<2} [{}] unnormalised: {dirty:?} (all: {:?})",
                if dirty.is_empty() { "pass" } else { "FAIL" },
                names_of(&rows),
            ));
        }
    }

    assert!(
        !failed,
        "a `--relative` operand's DOTDIR marker must be normalised out of every \
         transmitted name (upstream flist.c:2642-2657); a name that keeps it \
         sorts against its own children and desynchronises the NDX \
         stream:\n{}",
        report.join("\n"),
    );
}

/// The marker still decides FOLLOW vs LSTAT. `symdir` stays a symlink;
/// `symdir/` and `symdir/.` become the directory whose contents they name,
/// under the operand's own name rather than `realdir`.
#[test]
fn relative_symlink_operand_follows_only_with_the_marker() {
    // (suffix, is the operand expected to be sent as a symlink)
    let cases: [(&str, bool); 3] = [("", true), ("/", false), ("/.", false)];

    let mut report = Vec::new();
    let mut failed = false;
    for (suffix, want_symlink) in cases {
        let scratch = TempDir::new().expect("tempdir");
        build_source_tree(&scratch);
        let operand = PathBuf::from(format!("{}/symdir{suffix}", scratch.path().display()));

        let rows = scoped_flist(&scratch, &operand);
        let got = names_of(&rows);
        let expected: Vec<&str> = if want_symlink {
            vec!["symdir"]
        } else {
            vec!["symdir", "symdir/f.txt", "symdir/sub", "symdir/sub/g.txt"]
        };
        let sent_as_symlink = rows
            .iter()
            .any(|(n, ty)| n == "symdir" && *ty == FileType::Symlink);

        let row_ok = got == expected && sent_as_symlink == want_symlink;
        failed |= !row_ok;
        report.push(format!(
            "  symdir{suffix:<2} [{}] symlink={sent_as_symlink} (want {want_symlink})\n \
             left: {got:?}\n            right: {expected:?}",
            if row_ok { "pass" } else { "FAIL" },
        ));
    }

    assert!(
        !failed,
        "the DOTDIR/SLASH_ENDING marker, and only it, decides whether a \
         `--relative` symlink operand is followed (upstream flist.c:2697); the \
         followed contents still ride under the operand's own name:\n{}",
        report.join("\n"),
    );
}
