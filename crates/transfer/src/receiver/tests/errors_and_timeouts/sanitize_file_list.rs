//! `ReceiverContext::sanitize_file_list` trust gating: rejects absolute
//! paths and `..` traversal when `trust_sender=false`, leaves them in
//! place when `trust_sender=true`, and special-cases the `--relative`
//! flag that strips the leading slash from absolute wire paths.

use std::ffi::OsString;

use protocol::ProtocolVersion;
use protocol::flist::FileEntry;

use super::super::super::ReceiverContext;
use super::super::support::test_handshake;
use crate::config::ServerConfig;
use crate::flags::ParsedServerFlags;
use crate::role::ServerRole;

fn receiver_with_trust(entries: Vec<FileEntry>, trust_sender: bool) -> ReceiverContext {
    let handshake = test_handshake();
    let config = ServerConfig {
        role: ServerRole::Receiver,
        protocol: ProtocolVersion::try_from(32u8).unwrap(),
        flag_string: "-logDtpre.".to_owned(),
        trust_sender,
        args: vec![OsString::from(".")],
        ..Default::default()
    };
    let mut ctx = ReceiverContext::new(&handshake, config);
    ctx.file_list = entries;
    ctx
}

fn receiver_with_trust_and_relative(
    entries: Vec<FileEntry>,
    trust_sender: bool,
) -> ReceiverContext {
    let handshake = test_handshake();
    let config = ServerConfig {
        role: ServerRole::Receiver,
        protocol: ProtocolVersion::try_from(32u8).unwrap(),
        flag_string: "-logDtpre.".to_owned(),
        trust_sender,
        flags: ParsedServerFlags {
            relative: true,
            ..Default::default()
        },
        args: vec![OsString::from(".")],
        ..Default::default()
    };
    let mut ctx = ReceiverContext::new(&handshake, config);
    ctx.file_list = entries;
    ctx
}

#[test]
fn safe_paths_kept_when_untrusted() {
    let entries = vec![
        FileEntry::new_file("hello.txt".into(), 10, 0o644),
        FileEntry::new_file("subdir/nested.txt".into(), 20, 0o644),
    ];
    let mut ctx = receiver_with_trust(entries, false);
    let removed = ctx.sanitize_file_list();
    assert_eq!(removed, 0);
    assert_eq!(ctx.file_list.len(), 2);
}

#[test]
fn absolute_path_rejected_when_untrusted() {
    let entries = vec![
        FileEntry::new_file("safe.txt".into(), 10, 0o644),
        FileEntry::new_file("/etc/passwd".into(), 20, 0o644),
    ];
    let mut ctx = receiver_with_trust(entries, false);
    let removed = ctx.sanitize_file_list();
    assert_eq!(removed, 1);
    assert_eq!(ctx.file_list.len(), 1);
    assert_eq!(ctx.file_list[0].path().to_str().unwrap(), "safe.txt");
}

#[test]
fn dot_dot_path_rejected_when_untrusted() {
    let entries = vec![
        FileEntry::new_file("ok.txt".into(), 10, 0o644),
        FileEntry::new_file("../escape.txt".into(), 20, 0o644),
        FileEntry::new_file("sub/../../escape2.txt".into(), 30, 0o644),
    ];
    let mut ctx = receiver_with_trust(entries, false);
    let removed = ctx.sanitize_file_list();
    assert_eq!(removed, 2);
    assert_eq!(ctx.file_list.len(), 1);
    assert_eq!(ctx.file_list[0].path().to_str().unwrap(), "ok.txt");
}

#[test]
fn absolute_path_allowed_when_trusted() {
    let entries = vec![
        FileEntry::new_file("safe.txt".into(), 10, 0o644),
        FileEntry::new_file("/etc/passwd".into(), 20, 0o644),
    ];
    let mut ctx = receiver_with_trust(entries, true);
    let removed = ctx.sanitize_file_list();
    assert_eq!(removed, 0);
    assert_eq!(ctx.file_list.len(), 2);
}

#[test]
fn dot_dot_path_allowed_when_trusted() {
    let entries = vec![
        FileEntry::new_file("ok.txt".into(), 10, 0o644),
        FileEntry::new_file("../escape.txt".into(), 20, 0o644),
    ];
    let mut ctx = receiver_with_trust(entries, true);
    let removed = ctx.sanitize_file_list();
    assert_eq!(removed, 0);
    assert_eq!(ctx.file_list.len(), 2);
}

#[test]
fn absolute_path_allowed_with_relative_flag() {
    // upstream: absolute paths are allowed when --relative is active
    let entries = vec![FileEntry::new_file("/rooted/file.txt".into(), 10, 0o644)];
    let mut ctx = receiver_with_trust_and_relative(entries, false);
    let removed = ctx.sanitize_file_list();
    assert_eq!(removed, 0);
    // Leading slashes are stripped in --relative mode
    assert!(!ctx.file_list[0].path().has_root());
}

#[test]
fn all_unsafe_entries_removed() {
    let entries = vec![
        FileEntry::new_file("/abs1".into(), 10, 0o644),
        FileEntry::new_file("../up1".into(), 20, 0o644),
        FileEntry::new_file("/abs2".into(), 30, 0o644),
    ];
    let mut ctx = receiver_with_trust(entries, false);
    let removed = ctx.sanitize_file_list();
    assert_eq!(removed, 3);
    assert!(ctx.file_list.is_empty());
}

#[test]
fn trust_sender_skips_all_checks() {
    let entries = vec![
        FileEntry::new_file("/abs".into(), 10, 0o644),
        FileEntry::new_file("../dotdot".into(), 20, 0o644),
        FileEntry::new_file("a/../../escape".into(), 30, 0o644),
        FileEntry::new_file("safe.txt".into(), 40, 0o644),
    ];
    let mut ctx = receiver_with_trust(entries, true);
    let removed = ctx.sanitize_file_list();
    assert_eq!(removed, 0);
    assert_eq!(ctx.file_list.len(), 4);
}

#[test]
fn empty_file_list_returns_zero() {
    let mut ctx = receiver_with_trust(vec![], false);
    let removed = ctx.sanitize_file_list();
    assert_eq!(removed, 0);

    let mut ctx_trusted = receiver_with_trust(vec![], true);
    let removed = ctx_trusted.sanitize_file_list();
    assert_eq!(removed, 0);
}

#[test]
fn directories_with_dot_dot_rejected() {
    let entries = vec![
        FileEntry::new_directory("../evil_dir".into(), 0o755),
        FileEntry::new_directory("safe_dir".into(), 0o755),
    ];
    let mut ctx = receiver_with_trust(entries, false);
    let removed = ctx.sanitize_file_list();
    assert_eq!(removed, 1);
    assert_eq!(ctx.file_list[0].path().to_str().unwrap(), "safe_dir");
}

/// On Windows, `Path::has_root()` is false for drive-relative paths such
/// as `C:foo`, but `dest_dir.join("C:foo")` discards `dest_dir` entirely
/// (`Path::join` semantics). Without an additional check, an untrusted
/// sender could escape the destination tree by emitting a wire path
/// starting with a drive letter, UNC prefix, or `\\?\` extended prefix.
#[cfg(windows)]
#[test]
fn windows_drive_relative_path_rejected_when_untrusted() {
    let entries = vec![
        FileEntry::new_file("safe.txt".into(), 10, 0o644),
        FileEntry::new_file("C:foo".into(), 20, 0o644),
        FileEntry::new_file(r"C:\absolute".into(), 30, 0o644),
        FileEntry::new_file(r"\\server\share\file".into(), 40, 0o644),
        FileEntry::new_file(r"\\?\C:\verbatim".into(), 50, 0o644),
    ];
    let mut ctx = receiver_with_trust(entries, false);
    let removed = ctx.sanitize_file_list();
    assert_eq!(removed, 4);
    assert_eq!(ctx.file_list.len(), 1);
    assert_eq!(ctx.file_list[0].path().to_str().unwrap(), "safe.txt");
}

#[cfg(windows)]
#[test]
fn windows_drive_relative_path_allowed_when_trusted() {
    let entries = vec![
        FileEntry::new_file("safe.txt".into(), 10, 0o644),
        FileEntry::new_file("C:foo".into(), 20, 0o644),
    ];
    let mut ctx = receiver_with_trust(entries, true);
    let removed = ctx.sanitize_file_list();
    assert_eq!(removed, 0);
    assert_eq!(ctx.file_list.len(), 2);
}
