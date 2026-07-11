//! Sender-side `--copy-dirlinks` (`-k`) file-list behaviour.
//!
//! With `--copy-dirlinks` the flist generator transmits a symlink whose target
//! is a directory as a real directory (using `stat`, descending into it),
//! while a symlink to a non-directory (a file) stays a symlink. This mirrors
//! upstream `link_stat()` (`flist.c:1362-1370`): `follow_dirlinks` follows only
//! symlinks-to-directories, unlike `--copy-links` (`-L`), which follows all
//! symlinks. Without `-k` every symlink is recorded unfollowed.
//!
//! # Upstream Reference
//!
//! - `flist.c:1362-1370` - `link_stat()` follows a symlink-to-dir when
//!   `follow_dirlinks` (set to `copy_dirlinks`) and the target is a directory.
//! - `options.c:687` / `options.c:2658-2659` - `-k` / compact `k`.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::symlink;
use std::path::Path;

use tempfile::TempDir;

use protocol::ProtocolVersion;
use protocol::flist::FileType;
use transfer::{
    GeneratorContext, HandshakeResult, ServerConfig, ServerRole, TransferPhase, TransferPipeline,
};

/// Builds a recursive sender (generator) config. When `copy_dirlinks` is set
/// the compact flag string carries the `k` letter parsed into
/// `flags.copy_dirlinks`. The `.iLsfxCIvu` suffix is the `-e` capability
/// string (the `L` there is a capability byte, not `--copy-links`).
fn generator_config(source_root: &Path, copy_dirlinks: bool) -> ServerConfig {
    let flag_string = if copy_dirlinks {
        "-rk.iLsfxCIvu".to_owned()
    } else {
        "-r.iLsfxCIvu".to_owned()
    };
    let mut config = ServerConfig::from_flag_string_and_args(
        ServerRole::Generator,
        flag_string,
        vec![source_root.to_path_buf().into_os_string()],
    )
    .expect("server config");
    config.connection.client_mode = true;
    assert_eq!(config.flags.copy_dirlinks, copy_dirlinks);
    // --copy-dirlinks must never imply --copy-links (which follows all links).
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

/// Populates `src` with a real directory, a plain file, a symlink-to-directory
/// and a symlink-to-file. Returns the `src` path.
fn build_source_tree(scratch: &TempDir) -> std::path::PathBuf {
    let src = scratch.path().join("src");
    let realdir = src.join("realdir");
    fs::create_dir_all(&realdir).expect("mkdir src/realdir");
    fs::write(realdir.join("f.txt"), b"payload").expect("write realdir/f.txt");
    fs::write(src.join("afile"), b"file contents").expect("write afile");
    symlink("realdir", src.join("dlink")).expect("plant dlink -> realdir");
    symlink("afile", src.join("flink")).expect("plant flink -> afile");
    src
}

/// With `-k`, the symlink-to-directory `dlink` is recorded as a real directory
/// and its contents are enumerated, while the symlink-to-file `flink` remains a
/// symlink.
#[test]
fn copy_dirlinks_transmits_symlink_to_dir_as_directory() {
    let scratch = TempDir::new().expect("tempdir");
    let src = build_source_tree(&scratch);

    let config = generator_config(&src, true);
    let handshake = test_handshake();
    let pipeline = test_pipeline();
    let mut ctx = GeneratorContext::new(&handshake, config, pipeline);
    ctx.build_file_list(&[src.clone()])
        .expect("build_file_list");

    let file_list = ctx.file_list();
    let find = |suffix: &str| file_list.iter().find(|e| e.name().ends_with(suffix));

    let dlink = find("dlink").expect("dlink entry present");
    assert_eq!(
        dlink.file_type(),
        FileType::Directory,
        "with -k a symlink-to-directory must be transmitted as a real directory"
    );

    // The followed directory's contents are enumerated beneath it, exactly as
    // upstream descends into a copy_dirlinks'd directory.
    assert!(
        file_list
            .iter()
            .any(|e| e.name().contains("dlink") && e.name().ends_with("f.txt")),
        "the contents of the followed symlink-to-directory must be enumerated: {:?}",
        file_list.iter().map(|e| e.name()).collect::<Vec<_>>()
    );

    // A symlink to a plain file is NOT a dirlink and stays a symlink.
    let flink = find("flink").expect("flink entry present");
    assert_eq!(
        flink.file_type(),
        FileType::Symlink,
        "with -k a symlink-to-file must remain a symlink (only dirlinks are followed)"
    );
}

/// Without `-k`, every symlink is recorded unfollowed - `dlink` stays a
/// symlink, proving the follow is gated on the flag.
#[test]
fn without_copy_dirlinks_symlink_to_dir_stays_symlink() {
    let scratch = TempDir::new().expect("tempdir");
    let src = build_source_tree(&scratch);

    let config = generator_config(&src, false);
    let handshake = test_handshake();
    let pipeline = test_pipeline();
    let mut ctx = GeneratorContext::new(&handshake, config, pipeline);
    ctx.build_file_list(&[src.clone()])
        .expect("build_file_list");

    let file_list = ctx.file_list();
    let dlink = file_list
        .iter()
        .find(|e| e.name().ends_with("dlink"))
        .expect("dlink entry present");
    assert_eq!(
        dlink.file_type(),
        FileType::Symlink,
        "without -k a symlink-to-directory must stay a symlink"
    );
}
