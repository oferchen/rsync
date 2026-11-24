//! The basis a recorded `--read-batch` row names, resolved against the
//! replaying invocation's options (upstream: receiver.c:1009-1053).
//!
//! `tests/read_batch_recorded_basis.rs` replays real rsync 3.5.2 batches; these
//! cells pin each arm without the oracle so every CI job covers them.

use std::path::{Path, PathBuf};

use protocol::FnameCmpType;

use super::super::ReceiverContext;
use super::super::wire::SenderAttrs;
use super::support::{test_config, test_handshake};
use crate::config::{ReferenceDirectory, ReferenceDirectoryKind, ServerConfig};

fn dest() -> PathBuf {
    PathBuf::from("dest")
}

fn relative() -> PathBuf {
    Path::new("sub").join("f")
}

fn row(fnamecmp_type: Option<FnameCmpType>, xname: Option<&[u8]>) -> SenderAttrs {
    SenderAttrs {
        fnamecmp_type,
        xname: xname.map(<[u8]>::to_vec),
        ..SenderAttrs::default()
    }
}

fn reference(path: &str) -> ReferenceDirectory {
    ReferenceDirectory {
        kind: ReferenceDirectoryKind::Copy,
        path: PathBuf::from(path),
        requested: PathBuf::from(path),
    }
}

fn resolve(config: ServerConfig, attrs: &SenderAttrs) -> std::io::Result<Option<PathBuf>> {
    let handshake = test_handshake();
    let ctx = ReceiverContext::new_for_test(&handshake, config);
    let relative = relative();
    let file_path = dest().join(&relative);
    ctx.recorded_basis(attrs, &relative, &file_path, &dest())
}

#[test]
fn fname_row_names_the_destination_itself() {
    for kind in [None, Some(FnameCmpType::Fname)] {
        assert_eq!(resolve(test_config(), &row(kind, None)).unwrap(), None);
    }
}

#[test]
fn partial_dir_row_names_the_file_under_the_partial_dir() {
    let mut config = test_config();
    config.partial_dir = Some(PathBuf::from(".p"));
    let named = resolve(config, &row(Some(FnameCmpType::PartialDir), None)).unwrap();
    assert_eq!(named, Some(dest().join("sub").join(".p").join("f")));
}

/// options.c:2627-2628 - `--delay-updates` alone stages through `.~tmp~`, so a
/// recorded partial-dir row resolves there.
#[test]
fn partial_dir_row_under_delay_updates_uses_the_implicit_dir() {
    let mut config = test_config();
    config.write.delay_updates = true;
    let named = resolve(config, &row(Some(FnameCmpType::PartialDir), None)).unwrap();
    assert_eq!(named, Some(dest().join("sub").join(".~tmp~").join("f")));
}

/// receiver.c:1009 - without a partial dir `partialptr` is `fname`, so the
/// row's basis is the destination itself.
#[test]
fn partial_dir_row_without_a_partial_dir_names_the_destination() {
    let named = resolve(test_config(), &row(Some(FnameCmpType::PartialDir), None)).unwrap();
    assert_eq!(named, None);
}

/// get_backup_name() - the default `~` suffix beside the file.
#[test]
fn backup_row_names_the_suffixed_backup() {
    let named = resolve(test_config(), &row(Some(FnameCmpType::Backup), None)).unwrap();
    assert_eq!(named, Some(dest().join("sub").join("f~")));
}

/// get_backup_name() - under `--backup-dir` the backup keeps the file's
/// relative path inside that directory and takes no default suffix.
#[test]
fn backup_row_honours_the_backup_dir() {
    let mut config = test_config();
    config.backup_dir = Some("bak".to_owned());
    let named = resolve(config, &row(Some(FnameCmpType::Backup), None)).unwrap();
    assert_eq!(named, Some(dest().join("bak").join("sub").join("f")));
}

#[test]
fn basis_dir_row_names_the_file_under_that_reference_dir() {
    let mut config = test_config();
    config.reference_directories = vec![reference("ref0"), reference("ref1")];
    let named = resolve(config, &row(Some(FnameCmpType::BasisDir(1)), None)).unwrap();
    assert_eq!(named, Some(Path::new("ref1").join("sub").join("f")));
}

/// receiver.c:1045-1049 - an index past `basis_dir_cnt` is a protocol error
/// (exit 2), not a silent fall back to the destination.
#[test]
fn basis_dir_row_past_the_reference_list_is_a_protocol_violation() {
    let mut config = test_config();
    config.reference_directories = vec![reference("ref0")];
    let err = resolve(config, &row(Some(FnameCmpType::BasisDir(1)), None)).unwrap_err();
    assert!(
        err.get_ref()
            .is_some_and(|inner| inner.is::<protocol::ProtocolViolation>()),
        "{err}"
    );
    assert_eq!(err.to_string(), "invalid basis_dir index: 1.");
}

/// receiver.c:1025-1028 - the fuzzy arm still goes through the shared wire
/// resolver, which refuses it with `--fuzzy` off.
#[test]
fn fuzzy_row_with_fuzzy_off_is_refused() {
    let err = resolve(
        test_config(),
        &row(Some(FnameCmpType::Fuzzy(0)), Some(b"f.old")),
    )
    .unwrap_err();
    assert!(
        err.to_string()
            .contains("refusing malicious fuzzy operation"),
        "{err}"
    );
}

#[test]
fn fuzzy_row_names_the_recorded_xname_beside_the_file() {
    let mut config = test_config();
    config.flags.fuzzy_level = 1;
    let named = resolve(config, &row(Some(FnameCmpType::Fuzzy(0)), Some(b"f.old"))).unwrap();
    assert_eq!(named, Some(dest().join("sub").join("f.old")));
}
