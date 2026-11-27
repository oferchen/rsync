//! The receiver names every non-regular entry it leaves uncreated.
//!
//! upstream: generator.c:2150-2155 - a symlink without `--links`
//! (generator.c:1985), a FIFO without `--specials` or a device without
//! `--devices` (generator.c:2072-2074) falls through to
//! `rprintf(FINFO, "skipping non-regular file \"%s\"\n", fname)` under
//! `INFO_GTE(NONREG, 1)`. The receiver used to skip the symlink and FIFO in
//! silence, so a push to an oc server printed nothing where 3.5.2 prints the
//! line.
use std::io::{self, Write};
use std::path::PathBuf;

use logging::{DiagnosticEvent, InfoFlag, VerbosityConfig, drain_events, init};
use protocol::ProtocolVersion;
use protocol::flist::FileEntry;

use super::super::ReceiverContext;
use super::support::test_handshake;
use crate::config::ServerConfig;
use crate::flags::ParsedServerFlags;
use crate::role::ServerRole;
use crate::writer::MsgInfoSender;

struct NullMsgInfoWriter;

impl Write for NullMsgInfoWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl MsgInfoSender for NullMsgInfoWriter {
    fn send_msg_info(&mut self, _data: &[u8]) -> io::Result<()> {
        Ok(())
    }
}

/// A receiver for `rsync -r`: no `--links`, `--devices` or `--specials`.
fn plain_recursive_config() -> ServerConfig {
    ServerConfig {
        role: ServerRole::Receiver,
        protocol: ProtocolVersion::try_from(32u8).unwrap(),
        flag_string: "-re.".to_owned(),
        flags: ParsedServerFlags {
            recursive: true,
            ..Default::default()
        },
        args: vec![std::ffi::OsString::from(".")],
        ..Default::default()
    }
}

/// One regular file between a device, a FIFO and a symlink, in file-list order.
fn mixed_list() -> Vec<FileEntry> {
    vec![
        FileEntry::new_file("a".into(), 4, 0o644),
        FileEntry::new_char_device("sub/dev".into(), 0o600, 1, 3),
        FileEntry::new_fifo("sub/fifo".into(), 0o644),
        FileEntry::new_symlink("sub/lnk".into(), 0o777, "../a".into()),
    ]
}

/// Runs the pass at default verbosity (or as `verbosity` sets it) and returns
/// the NONREG lines, with every destination left as it was found.
fn notices(config: ServerConfig, verbosity: VerbosityConfig, renamed: Option<&str>) -> Vec<String> {
    let tmp = tempfile::tempdir().expect("tempdir");
    let handshake = test_handshake();
    let mut ctx = ReceiverContext::new_for_test(&handshake, config);
    ctx.file_list = mixed_list().into();
    ctx.single_file_renamed_from = renamed.map(PathBuf::from);
    init(verbosity);
    drain_events();
    ctx.create_specials(tmp.path(), None, None, None, &mut NullMsgInfoWriter)
        .expect("the pass succeeds");
    for name in ["sub/fifo", "sub/lnk"] {
        assert!(
            std::fs::symlink_metadata(tmp.path().join(name)).is_err(),
            "a skipped {name} must not be created"
        );
    }
    drain_events()
        .into_iter()
        .filter_map(|event| match event {
            DiagnosticEvent::Bytes {
                flag: InfoFlag::Nonreg,
                message,
                ..
            } => Some(String::from_utf8(message).expect("UTF-8 notice")),
            _ => None,
        })
        .collect()
}

#[test]
fn every_entry_left_uncreated_is_named_in_file_list_order() {
    assert_eq!(
        notices(
            plain_recursive_config(),
            VerbosityConfig::from_verbose_level(0),
            None
        ),
        [
            "skipping non-regular file \"sub/dev\"",
            "skipping non-regular file \"sub/fifo\"",
            "skipping non-regular file \"sub/lnk\"",
        ]
    );
}

/// upstream: the generator runs under `--dry-run` too, so the line still
/// prints.
#[test]
fn a_dry_run_names_the_same_entries() {
    let mut config = plain_recursive_config();
    config.flags.dry_run = true;
    assert_eq!(
        notices(config, VerbosityConfig::from_verbose_level(0), None).len(),
        3
    );
}

/// upstream: options.c:257 - NONREG is the only row-0 word, so
/// `--info=nonreg0` is the one switch that drops the line at default
/// verbosity.
#[test]
fn info_nonreg0_drops_every_line() {
    let mut verbosity = VerbosityConfig::from_verbose_level(0);
    verbosity.info.nonreg = 0;
    assert_eq!(
        notices(plain_recursive_config(), verbosity, None),
        Vec::<String>::new()
    );
}

/// `--drop-D` withholds creation, so a FIFO sent with `--specials` still
/// reaches the line (generator.c:2072 `!drop_devices && ...`).
#[test]
fn drop_devices_sends_a_preserved_fifo_to_the_line() {
    let mut config = plain_recursive_config();
    config.flags.links = true;
    config.flags.specials = true;
    config.flags.drop_devices = true;
    assert_eq!(
        notices(config, VerbosityConfig::from_verbose_level(0), None),
        [
            "skipping non-regular file \"sub/dev\"",
            "skipping non-regular file \"sub/fifo\"",
        ]
    );
}

/// upstream: generator.c:2152-2153 - under `solo_file` the line names the
/// file-list entry, not the destination operand the receiver renamed it to.
#[test]
fn a_renamed_lone_entry_is_named_by_its_file_list_name() {
    let mut config = plain_recursive_config();
    config.flags.links = true;
    config.flags.devices = true;
    let found = notices(config, VerbosityConfig::from_verbose_level(0), Some("orig"));
    assert!(
        found
            .iter()
            .all(|line| line == "skipping non-regular file \"orig\""),
        "{found:?}"
    );
    assert!(found.iter().any(|line| line.contains("orig")), "{found:?}");
}
