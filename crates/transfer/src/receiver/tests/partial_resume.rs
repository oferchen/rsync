//! Partial-transfer and resume surface: `TempFileGuard` lifecycle,
//! `--relative` parent-directory creation, and reference-directory lookups
//! used to seed basis files for resumes.

use std::ffi::OsString;

use protocol::ProtocolVersion;
use protocol::flist::FileEntry;

use super::support::{REDO_CHECKSUM_LENGTH, test_handshake};
use crate::config::ServerConfig;
use crate::flags::ParsedServerFlags;
use crate::role::ServerRole;
use crate::temp_guard::TempFileGuard;

use super::super::ReceiverContext;

#[test]
fn temp_file_guard_cleans_up_on_drop() {
    use tempfile::TempDir;

    let temp_dir = TempDir::new().unwrap();
    let temp_path = temp_dir.path().join("test.tmp");

    std::fs::write(&temp_path, b"test data").unwrap();
    assert!(temp_path.exists());

    {
        let _guard = TempFileGuard::new(temp_path.clone());
        // Guard goes out of scope here, should delete file
    }

    assert!(!temp_path.exists());
}

#[test]
fn temp_file_guard_keeps_file_when_marked() {
    use tempfile::TempDir;

    let temp_dir = TempDir::new().unwrap();
    let temp_path = temp_dir.path().join("test.tmp");

    std::fs::write(&temp_path, b"test data").unwrap();
    assert!(temp_path.exists());

    {
        let mut guard = TempFileGuard::new(temp_path.clone());
        guard.keep(); // Mark as successful
    }

    assert!(temp_path.exists());
}

#[test]
fn basis_file_result_is_empty_when_no_signature() {
    use super::super::basis::BasisFileResult;

    let result = BasisFileResult {
        signature: None,
        basis_path: None,
        fnamecmp_type: protocol::FnameCmpType::Fname,
    };
    assert!(result.is_empty());
}

#[test]
fn basis_file_result_is_not_empty_when_has_signature() {
    use super::super::basis::BasisFileResult;
    use engine::delta::SignatureLayout;
    use engine::signature::FileSignature;
    use std::num::NonZeroU32;
    use std::path::PathBuf;

    let layout =
        SignatureLayout::from_raw_parts(NonZeroU32::new(512).unwrap(), 0, 0, REDO_CHECKSUM_LENGTH);
    let signature = FileSignature::from_raw_parts(layout, vec![], 0);

    let result = BasisFileResult {
        signature: Some(signature),
        basis_path: Some(PathBuf::from("/tmp/basis")),
        fnamecmp_type: protocol::FnameCmpType::Fname,
    };
    assert!(!result.is_empty());
}

#[test]
fn try_reference_directories_finds_file_in_first_directory() {
    use super::super::basis::try_reference_directories;
    use crate::config::{ReferenceDirectory, ReferenceDirectoryKind};

    let ref_dir1 = test_support::create_tempdir();
    let ref_dir2 = test_support::create_tempdir();

    let test_file = ref_dir1.path().join("subdir/test.txt");
    std::fs::create_dir_all(test_file.parent().unwrap()).unwrap();
    std::fs::write(&test_file, b"test content from ref1").unwrap();

    let ref_dirs = vec![
        ReferenceDirectory {
            kind: ReferenceDirectoryKind::Compare,
            path: ref_dir1.path().to_path_buf(),
        },
        ReferenceDirectory {
            kind: ReferenceDirectoryKind::Link,
            path: ref_dir2.path().to_path_buf(),
        },
    ];

    let relative_path = std::path::Path::new("subdir/test.txt");
    let result = try_reference_directories(relative_path, &ref_dirs);

    assert!(result.is_some());
    let (_, size, path) = result.unwrap();
    assert_eq!(size, 22);
    assert_eq!(path, test_file);
}

#[test]
fn try_reference_directories_finds_file_in_second_directory() {
    use super::super::basis::try_reference_directories;
    use crate::config::{ReferenceDirectory, ReferenceDirectoryKind};

    let ref_dir1 = test_support::create_tempdir();
    let ref_dir2 = test_support::create_tempdir();

    let test_file = ref_dir2.path().join("test.txt");
    std::fs::write(&test_file, b"test content from ref2").unwrap();

    let ref_dirs = vec![
        ReferenceDirectory {
            kind: ReferenceDirectoryKind::Compare,
            path: ref_dir1.path().to_path_buf(),
        },
        ReferenceDirectory {
            kind: ReferenceDirectoryKind::Copy,
            path: ref_dir2.path().to_path_buf(),
        },
    ];

    let relative_path = std::path::Path::new("test.txt");
    let result = try_reference_directories(relative_path, &ref_dirs);

    assert!(result.is_some());
    let (_, size, path) = result.unwrap();
    assert_eq!(size, 22);
    assert_eq!(path, test_file);
}

#[test]
fn try_reference_directories_returns_none_when_not_found() {
    use super::super::basis::try_reference_directories;
    use crate::config::{ReferenceDirectory, ReferenceDirectoryKind};

    let ref_dir = test_support::create_tempdir();

    let ref_dirs = vec![ReferenceDirectory {
        kind: ReferenceDirectoryKind::Link,
        path: ref_dir.path().to_path_buf(),
    }];

    let relative_path = std::path::Path::new("nonexistent.txt");
    let result = try_reference_directories(relative_path, &ref_dirs);

    assert!(result.is_none());
}

#[test]
fn try_reference_directories_empty_list_returns_none() {
    use super::super::basis::try_reference_directories;
    use crate::config::ReferenceDirectory;

    let ref_dirs: Vec<ReferenceDirectory> = vec![];
    let relative_path = std::path::Path::new("test.txt");
    let result = try_reference_directories(relative_path, &ref_dirs);

    assert!(result.is_none());
}

mod relative_parents {
    use super::*;

    fn receiver_with_relative(entries: Vec<FileEntry>) -> ReceiverContext {
        let handshake = test_handshake();
        let config = ServerConfig {
            role: ServerRole::Receiver,
            protocol: ProtocolVersion::try_from(32u8).unwrap(),
            flag_string: "-logDtpRe.".to_owned(),
            flags: ParsedServerFlags {
                relative: true,
                ..Default::default()
            },
            args: vec![OsString::from(".")],
            ..Default::default()
        };
        let mut ctx = ReceiverContext::new_for_test(&handshake, config);
        ctx.file_list = entries;
        ctx
    }

    fn receiver_without_relative(entries: Vec<FileEntry>) -> ReceiverContext {
        let handshake = test_handshake();
        let config = ServerConfig {
            role: ServerRole::Receiver,
            protocol: ProtocolVersion::try_from(32u8).unwrap(),
            flag_string: "-logDtpre.".to_owned(),
            args: vec![OsString::from(".")],
            ..Default::default()
        };
        let mut ctx = ReceiverContext::new_for_test(&handshake, config);
        ctx.file_list = entries;
        ctx
    }

    #[test]
    fn ensure_relative_parents_creates_missing_dirs() {
        let tmp = test_support::create_tempdir();
        let dest = tmp.path();

        let entries = vec![FileEntry::new_file("a/b/c/file.txt".into(), 100, 0o644)];
        let ctx = receiver_with_relative(entries);

        ctx.ensure_relative_parents(dest);

        assert!(dest.join("a").is_dir());
        assert!(dest.join("a/b").is_dir());
        assert!(dest.join("a/b/c").is_dir());
    }

    #[test]
    fn ensure_relative_parents_handles_multiple_entries_shared_prefix() {
        let tmp = test_support::create_tempdir();
        let dest = tmp.path();

        let entries = vec![
            FileEntry::new_file("src/lib/mod.rs".into(), 50, 0o644),
            FileEntry::new_file("src/lib/util.rs".into(), 75, 0o644),
            FileEntry::new_file("src/bin/main.rs".into(), 200, 0o644),
        ];
        let ctx = receiver_with_relative(entries);

        ctx.ensure_relative_parents(dest);

        assert!(dest.join("src").is_dir());
        assert!(dest.join("src/lib").is_dir());
        assert!(dest.join("src/bin").is_dir());
    }

    #[test]
    fn ensure_relative_parents_noop_without_relative_flag() {
        let tmp = test_support::create_tempdir();
        let dest = tmp.path();

        let entries = vec![FileEntry::new_file("a/b/file.txt".into(), 100, 0o644)];
        let ctx = receiver_without_relative(entries);

        ctx.ensure_relative_parents(dest);

        assert!(!dest.join("a").exists());
    }

    #[test]
    fn ensure_relative_parents_skips_dot_path() {
        let tmp = test_support::create_tempdir();
        let dest = tmp.path();

        let entries = vec![
            FileEntry::new_directory(".".into(), 0o755),
            FileEntry::new_file("file.txt".into(), 100, 0o644),
        ];
        let ctx = receiver_with_relative(entries);

        ctx.ensure_relative_parents(dest);
    }

    #[test]
    fn ensure_relative_parents_handles_directory_entries() {
        let tmp = test_support::create_tempdir();
        let dest = tmp.path();

        let entries = vec![FileEntry::new_directory("a/b/c".into(), 0o755)];
        let ctx = receiver_with_relative(entries);

        ctx.ensure_relative_parents(dest);

        assert!(dest.join("a").is_dir());
        assert!(dest.join("a/b").is_dir());
        // "a/b/c" is NOT created by ensure_relative_parents (it's a dir entry,
        // handled by create_directories / create_directory_incremental)
        assert!(!dest.join("a/b/c").exists());
    }

    #[test]
    fn ensure_relative_parents_existing_dirs_not_clobbered() {
        let tmp = test_support::create_tempdir();
        let dest = tmp.path();

        std::fs::create_dir_all(dest.join("a/b")).unwrap();
        std::fs::write(dest.join("a/b/existing.txt"), "hello").unwrap();

        let entries = vec![FileEntry::new_file("a/b/c/new.txt".into(), 100, 0o644)];
        let ctx = receiver_with_relative(entries);

        ctx.ensure_relative_parents(dest);

        assert_eq!(
            std::fs::read_to_string(dest.join("a/b/existing.txt")).unwrap(),
            "hello"
        );
        assert!(dest.join("a/b/c").is_dir());
    }

    #[test]
    fn ensure_relative_parents_dry_run_creates_nothing() {
        let tmp = test_support::create_tempdir();
        let dest = tmp.path();

        let handshake = test_handshake();
        let config = ServerConfig {
            role: ServerRole::Receiver,
            protocol: ProtocolVersion::try_from(32u8).unwrap(),
            flag_string: "-logDtpRne.".to_owned(),
            flags: ParsedServerFlags {
                relative: true,
                dry_run: true,
                ..Default::default()
            },
            args: vec![OsString::from(".")],
            ..Default::default()
        };
        let mut ctx = ReceiverContext::new_for_test(&handshake, config);
        ctx.file_list = vec![FileEntry::new_file(
            "deep/nested/file.txt".into(),
            100,
            0o644,
        )];

        ctx.ensure_relative_parents(dest);

        assert!(!dest.join("deep").exists());
    }

    #[test]
    fn ensure_relative_parents_single_component_path() {
        let tmp = test_support::create_tempdir();
        let dest = tmp.path();

        let entries = vec![FileEntry::new_file("file.txt".into(), 100, 0o644)];
        let ctx = receiver_with_relative(entries);

        ctx.ensure_relative_parents(dest);

        let dir_entries: Vec<_> = std::fs::read_dir(dest).unwrap().collect();
        assert!(dir_entries.is_empty());
    }
}
