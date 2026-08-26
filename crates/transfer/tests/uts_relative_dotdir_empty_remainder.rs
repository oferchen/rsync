//! `--relative` operands whose `/./` remainder is empty keep the DOTDIR marker.
//!
//! Upstream splits a `--relative` operand on the first `/./`; when the part
//! after the anchor is empty it is forced to `"."` with `DOTDIR_NAME`
//! (`flist.c:2670-2673`), which then makes `link_stat()` follow the operand
//! unconditionally (`flist.c:2697` - `copy_dirlinks || name_type !=
//! NORMAL_NAME`). Losing the marker turns `-R <symlinked-dir>/./` into an
//! `lstat` that records the operand as a symlink under its own basename
//! instead of descending into the directory it names.
//!
//! Both operand shapes are pinned. `<dir>/./.` (remainder `"."`) never lost the
//! marker; it is here so the pin cannot pass by making the `"."` join
//! unconditional.
//!
//! # Upstream Reference
//!
//! - `rsync-3.5.0/flist.c:2670-2673` - empty remainder becomes `"."` /
//!   `DOTDIR_NAME`.
//! - `rsync-3.5.0/flist.c:2697` - `link_stat(fbuf, &st, copy_dirlinks ||
//!   name_type != NORMAL_NAME)`.

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
        "the follow must come from the DOTDIR marker, not from -k"
    );
    assert!(!config.flags.copy_links);
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

/// `<scratch>/realdir/f.txt` plus `<scratch>/sub -> realdir`.
fn build_source_tree(scratch: &TempDir) -> PathBuf {
    let realdir = scratch.path().join("realdir");
    fs::create_dir_all(&realdir).expect("mkdir realdir");
    fs::write(realdir.join("f.txt"), b"payload").expect("write realdir/f.txt");
    symlink("realdir", scratch.path().join("sub")).expect("plant sub -> realdir");
    scratch.path().join("sub")
}

/// Builds the flist for `operand` and returns the transmitted names paired with
/// their file types.
fn flist_for(operand: &Path) -> Vec<(String, FileType)> {
    let config = generator_config(operand);
    let handshake = test_handshake();
    let pipeline = test_pipeline();
    let mut ctx = GeneratorContext::new(&handshake, config, pipeline);
    ctx.build_file_list(&[operand.to_path_buf()])
        .expect("build_file_list");
    ctx.file_list()
        .iter()
        .map(|e| (e.name().to_owned(), e.file_type()))
        .collect()
}

#[test]
fn relative_dotdir_operand_follows_symlinked_dir_for_every_remainder() {
    // (suffix appended to `<scratch>/sub`, what the remainder exercises)
    let cases: [(&str, &str); 2] = [
        ("/./", "empty remainder - upstream forces it to \".\""),
        ("/./.", "remainder \".\" - already carried the marker"),
    ];

    // Every row is evaluated; a failing row must not hide the verdict of the
    // others, or the pin cannot distinguish "both shapes broken" from "only the
    // empty remainder is".
    let mut report = Vec::new();
    let mut failed = false;
    for (suffix, why) in cases {
        let scratch = TempDir::new().expect("tempdir");
        let sub = build_source_tree(&scratch);
        let operand = PathBuf::from(format!("{}{suffix}", sub.display()));

        let entries = flist_for(&operand);
        let sent_as_symlink = entries
            .iter()
            .any(|(name, ty)| name.ends_with("sub") && *ty == FileType::Symlink);
        let delivered_contents = entries
            .iter()
            .any(|(name, ty)| name.ends_with("f.txt") && *ty == FileType::Regular);

        let row_ok = !sent_as_symlink && delivered_contents;
        failed |= !row_ok;
        report.push(format!(
            "  sub{suffix:<5} [{}] {why}: sent_as_symlink={sent_as_symlink} \
             delivered_contents={delivered_contents} names={:?}",
            if row_ok { "pass" } else { "FAIL" },
            entries.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(),
        ));
    }

    assert!(
        !failed,
        "a `/./` operand must follow the symlinked directory it names and \
         deliver its CONTENTS, never send it as a symlink under its own \
         basename (upstream flist.c:2670-2673 + flist.c:2697):\n{}",
        report.join("\n"),
    );
}
