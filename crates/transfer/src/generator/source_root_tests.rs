//! A non-daemon sender opens file content beneath the explicit source root it
//! recorded while building the file list.
//!
//! Each test builds a real file list, then swaps part of the tree the way an
//! attacker who owns a subdirectory of the source would between the scan and
//! the read, and opens the listed file through the production
//! [`GeneratorContext::source_open`] policy.
//!
//! upstream: `flist.c:2967` records the root, `sender.c:694-704` opens beneath it.

use std::ffi::OsString;
use std::io::Read;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use protocol::ProtocolVersion;

use super::GeneratorContext;
use crate::config::ServerConfig;
use crate::handshake::HandshakeResult;
use crate::role::ServerRole;

const SECRET: &str = "outside-secret";

fn handshake() -> HandshakeResult {
    HandshakeResult {
        protocol: ProtocolVersion::try_from(32u8).unwrap(),
        buffered: Vec::new(),
        compat_exchanged: false,
        client_args: None,
        io_timeout: None,
        negotiated_algorithms: None,
        compat_flags: None,
        checksum_seed: 0,
    }
}

fn config(relative: bool) -> ServerConfig {
    let mut config = ServerConfig {
        role: ServerRole::Generator,
        protocol: ProtocolVersion::try_from(32u8).unwrap(),
        flag_string: "-logDtpre.".to_owned(),
        args: vec![OsString::from(".")],
        ..Default::default()
    };
    config.flags.recursive = true;
    config.flags.relative = relative;
    config
}

/// `base/src/sub/f` holds in-tree data; `base/outside` mirrors the layout
/// with the secret the attacker wants the sender to read.
fn tree() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let base = tmp.path().canonicalize().expect("canonical tempdir");
    std::fs::create_dir_all(base.join("src/sub")).expect("mkdir src/sub");
    std::fs::create_dir_all(base.join("outside/sub")).expect("mkdir outside/sub");
    std::fs::write(base.join("src/sub/f"), "inside").expect("write inside");
    std::fs::write(base.join("outside/f"), SECRET).expect("write secret");
    std::fs::write(base.join("outside/sub/f"), SECRET).expect("write secret");
    (tmp, base)
}

fn listed(ctx: &GeneratorContext, suffix: &str) -> PathBuf {
    let ndx = ctx
        .file_list()
        .iter()
        .position(|entry| entry.path().ends_with(suffix))
        .unwrap_or_else(|| panic!("{suffix} not in the file list"));
    ctx.reconstruct_source_path(ndx)
}

/// Opens `path` through the sender policy and returns what it read, or the
/// open error.
fn read_through_sender(ctx: &GeneratorContext, path: &Path) -> std::io::Result<String> {
    let mut text = String::new();
    ctx.source_open().open(path)?.read_to_string(&mut text)?;
    Ok(text)
}

fn swap_sub_for_escaping_symlink(base: &Path) {
    std::fs::rename(base.join("src/sub"), base.join("src/.realsub")).expect("move sub");
    symlink("../outside", base.join("src/sub")).expect("plant symlink");
}

#[test]
fn a_parent_swapped_after_the_scan_is_not_followed_out_of_the_root() {
    let (_tmp, base) = tree();
    let mut operand = base.join("src").into_os_string();
    operand.push("/");
    let mut ctx = GeneratorContext::new_for_test(&handshake(), config(false));
    ctx.build_file_list(&[PathBuf::from(operand)])
        .expect("file list");
    let path = listed(&ctx, "sub/f");
    assert_eq!(
        read_through_sender(&ctx, &path).expect("pre-swap read"),
        "inside"
    );

    swap_sub_for_escaping_symlink(&base);
    let outcome = read_through_sender(&ctx, &path);
    assert!(
        outcome.is_err(),
        "sender read through the swapped parent: {outcome:?}"
    );
}

#[test]
fn a_relative_operand_root_swapped_after_the_scan_is_refused_with_eloop() {
    let (_tmp, base) = tree();
    let mut ctx = GeneratorContext::new_for_test(&handshake(), config(true));
    ctx.build_file_list(&[base.join("src/sub/f")])
        .expect("file list");
    let path = listed(&ctx, "sub/f");

    swap_sub_for_escaping_symlink(&base);
    let error = read_through_sender(&ctx, &path).expect_err("swapped root must be refused");
    assert_eq!(error.raw_os_error(), Some(libc::ELOOP), "{error}");
}

#[test]
fn a_root_replaced_after_the_scan_is_refused_with_eloop() {
    let (_tmp, base) = tree();
    let mut ctx = GeneratorContext::new_for_test(&handshake(), config(false));
    ctx.build_file_list(&[base.join("src")]).expect("file list");
    let path = listed(&ctx, "sub/f");

    std::fs::rename(base.join("src"), base.join("src.real")).expect("move root");
    symlink(base.join("outside"), base.join("src")).expect("plant root symlink");
    let error = read_through_sender(&ctx, &path).expect_err("replaced root must be refused");
    assert_eq!(error.raw_os_error(), Some(libc::ELOOP), "{error}");
}

#[test]
fn copy_links_keeps_following_symlinks() {
    let (_tmp, base) = tree();
    let mut config = config(false);
    config.flags.copy_links = true;
    let mut ctx = GeneratorContext::new_for_test(&handshake(), config);
    ctx.build_file_list(&[base.join("src")]).expect("file list");
    let path = listed(&ctx, "sub/f");

    swap_sub_for_escaping_symlink(&base);
    // upstream: sender.c:707-709 - a symlink-following mode takes
    // do_open_checklinks(), the unconfined open.
    assert_eq!(read_through_sender(&ctx, &path).expect("followed"), SECRET);
}
