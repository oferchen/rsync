//! `--existing` / `--ignore-existing` on the non-regular receiver passes.
//!
//! upstream: generator.c:1757-1806 recv_generator() tests both options on the
//! destination `lstat` for EVERY file type before any type-specific branch, so
//! a symlink, FIFO or directory is skipped under exactly the conditions a
//! regular file is. A receiver that gates only regular files replaces links and
//! nodes `--ignore-existing` promised to leave alone, and creates entries
//! `--existing` promised never to create.

use std::fs;
use std::io::{self, Write};
use std::path::Path;

use logging::{InfoFlag, VerbosityConfig};
use metadata::MetadataOptions;
use protocol::ProtocolVersion;
use protocol::flist::FileEntry;

use super::super::ReceiverContext;
use super::support::test_handshake;
use crate::config::ServerConfig;
use crate::flags::ParsedServerFlags;
use crate::role::ServerRole;
use crate::writer::MsgInfoSender;

/// Records every `MSG_INFO` line a server-mode receiver emits.
#[derive(Default)]
struct CaptureWriter {
    lines: Vec<String>,
}

impl Write for CaptureWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl MsgInfoSender for CaptureWriter {
    fn send_msg_info(&mut self, data: &[u8]) -> io::Result<()> {
        self.lines.push(String::from_utf8_lossy(data).into_owned());
        Ok(())
    }
}

/// A `-rlD` server receiver with one of the two existence options set.
fn receiver(existing_only: bool, ignore_existing: bool, files: Vec<FileEntry>) -> ReceiverContext {
    let mut cfg = VerbosityConfig::default();
    cfg.info.set(InfoFlag::Skip, 1);
    logging::init(cfg);

    let mut config = ServerConfig {
        role: ServerRole::Receiver,
        protocol: ProtocolVersion::try_from(32u8).unwrap(),
        flag_string: "-rlDe.".to_owned(),
        flags: ParsedServerFlags {
            recursive: true,
            links: true,
            devices: true,
            specials: true,
            ..Default::default()
        },
        args: vec![std::ffi::OsString::from(".")],
        ..Default::default()
    };
    config.connection.client_mode = false;
    config.file_selection.existing_only = existing_only;
    config.file_selection.ignore_existing = ignore_existing;
    let mut ctx = ReceiverContext::new_for_test(&test_handshake(), config);
    ctx.file_list = files;
    ctx
}

/// Runs the directory, symlink and special passes, returning the notices
/// sorted: each pass reports in flist order, but the passes run one after
/// another.
fn create_non_regular(ctx: &ReceiverContext, dest: &Path) -> Vec<String> {
    let mut writer = CaptureWriter::default();
    ctx.create_directories(
        dest,
        &MetadataOptions::default(),
        None,
        None,
        &mut writer,
        None,
    )
    .expect("create_directories");
    ctx.create_symlinks(dest, None, &mut writer)
        .expect("create_symlinks");
    ctx.create_specials(dest, None, &mut writer)
        .expect("create_specials");
    let mut lines = writer.lines;
    lines.sort();
    lines
}

/// upstream: generator.c:1784-1799 - every present destination that is not a
/// directory receiving a directory is left standing, with a `%s exists`
/// notice: the old symlink target, the regular file where a FIFO arrives, and
/// the regular file where a directory arrives all survive.
#[test]
fn ignore_existing_leaves_every_present_destination_type_standing() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dest = tmp.path();
    std::os::unix::fs::symlink("old_target", dest.join("link")).expect("dest link");
    fs::write(dest.join("fifo"), b"keep fifo").expect("dest file");
    fs::write(dest.join("dir"), b"keep dir").expect("dest file");

    let ctx = receiver(
        false,
        true,
        vec![
            FileEntry::new_directory("dir".into(), 0o755),
            FileEntry::new_fifo("fifo".into(), 0o644),
            FileEntry::new_symlink("link".into(), 0o777, "new_target".into()),
        ],
    );
    let lines = create_non_regular(&ctx, dest);

    assert_eq!(
        fs::read_link(dest.join("link")).expect("link kept"),
        Path::new("old_target")
    );
    assert_eq!(
        fs::read(dest.join("fifo")).expect("file kept"),
        b"keep fifo"
    );
    assert_eq!(fs::read(dest.join("dir")).expect("file kept"), b"keep dir");
    assert_eq!(
        lines,
        vec![
            "dir exists\n".to_owned(),
            "fifo exists\n".to_owned(),
            "link exists\n".to_owned(),
        ]
    );
}

/// upstream: generator.c:1784-1785 - a directory arriving over a directory is
/// merged into, not skipped, and draws no notice.
#[test]
fn ignore_existing_still_merges_into_an_existing_directory() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dest = tmp.path();
    fs::create_dir(dest.join("dir")).expect("dest dir");

    let ctx = receiver(
        false,
        true,
        vec![
            FileEntry::new_directory("dir".into(), 0o755),
            FileEntry::new_directory("dir/sub".into(), 0o755),
        ],
    );
    let lines = create_non_regular(&ctx, dest);

    assert!(dest.join("dir/sub").is_dir(), "new subdirectory created");
    assert!(lines.is_empty(), "lines = {lines:?}");
}

/// upstream: generator.c:1757-1772 - `--existing` creates no absent entry of
/// any type. A skipped directory becomes `skip_dir`, so its descendants are
/// skipped silently: only the directory itself is reported.
#[test]
fn existing_creates_no_absent_entry_of_any_type() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dest = tmp.path();

    let ctx = receiver(
        true,
        false,
        vec![
            FileEntry::new_directory("dir".into(), 0o755),
            FileEntry::new_symlink("dir/inner".into(), 0o777, "t".into()),
            FileEntry::new_fifo("fifo".into(), 0o644),
            FileEntry::new_symlink("link".into(), 0o777, "t".into()),
        ],
    );
    let lines = create_non_regular(&ctx, dest);

    for name in ["dir", "fifo", "link"] {
        assert!(
            fs::symlink_metadata(dest.join(name)).is_err(),
            "{name} must not be created"
        );
    }
    assert_eq!(
        lines,
        vec![
            "not creating new directory \"dir\"\n".to_owned(),
            "not creating new file \"fifo\"\n".to_owned(),
            "not creating new file \"link\"\n".to_owned(),
        ]
    );
}

/// upstream: generator.c:1757 - `--existing` still updates a present entry:
/// an existing symlink is repointed at the new target.
#[test]
fn existing_keeps_updating_present_entries() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dest = tmp.path();
    std::os::unix::fs::symlink("old_target", dest.join("link")).expect("dest link");

    let ctx = receiver(
        true,
        false,
        vec![FileEntry::new_symlink(
            "link".into(),
            0o777,
            "new_target".into(),
        )],
    );
    let lines = create_non_regular(&ctx, dest);

    assert_eq!(
        fs::read_link(dest.join("link")).expect("link updated"),
        Path::new("new_target")
    );
    assert!(lines.is_empty(), "lines = {lines:?}");
}

/// upstream: generator.c:1745 - the gate reads `link_stat()`, so a dangling
/// symlink where a regular file arrives exists and `--ignore-existing` leaves
/// it standing; a following stat would call it absent and request the file.
#[test]
fn ignore_existing_treats_dangling_symlink_as_present() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dest = tmp.path();
    std::os::unix::fs::symlink("missing_target", dest.join("file")).expect("dest link");

    let ctx = receiver(
        false,
        true,
        vec![FileEntry::new_file("file".into(), 4, 0o644)],
    );
    let mut writer = CaptureWriter::default();
    let mut errors = Vec::new();
    let mut stats = crate::receiver::stats::TransferStats::default();
    let requested = ctx.build_files_to_transfer(
        &mut writer,
        dest,
        None,
        &MetadataOptions::default(),
        None,
        &mut errors,
        &mut stats,
        None,
        None,
    );

    assert!(requested.is_empty(), "requested = {requested:?}");
    assert_eq!(writer.lines, vec!["file exists\n".to_owned()]);
}
