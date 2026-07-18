//! Higher-level incremental receive flow: end-to-end reads via the
//! `next_ready` / `collect_sorted` / `Iterator` surface, the
//! `create_directory_incremental` helper used by the receive loop,
//! and feature-gated checks for `incremental-flist` invariants.

use std::io::Cursor;
use std::io::{Read, Write};

use protocol::ProtocolVersion;
use protocol::flist::FileEntry;
use protocol::stats::DeleteStats;

use super::super::super::ReceiverContext;
use super::super::super::directory::FailedDirectories;
use super::super::super::stats::TransferStats;
use super::super::support::{test_config, test_handshake};
use crate::pipeline::PipelineConfig;

#[test]
fn incremental_receiver_reads_entries() {
    let protocol = ProtocolVersion::try_from(32u8).unwrap();
    let mut data = Vec::new();
    let mut writer = protocol::flist::FileListWriter::new(protocol);

    let dir = FileEntry::new_directory("testdir".into(), 0o755);
    let file = FileEntry::new_file("testdir/file.txt".into(), 100, 0o644);

    writer.write_entry(&mut data, &dir).unwrap();
    writer.write_entry(&mut data, &file).unwrap();
    writer.write_end(&mut data, None).unwrap();

    let handshake = test_handshake();
    let config = test_config();
    let ctx = ReceiverContext::new_for_test(&handshake, config);

    let mut receiver = ctx.incremental_file_list_receiver(Cursor::new(&data[..]));

    // The directory has no parent dependency, so it is ready first.
    let entry1 = receiver.next_ready().unwrap().unwrap();
    assert!(entry1.is_dir());
    assert_eq!(entry1.name(), "testdir");

    // The file is released once its parent dir exists.
    let entry2 = receiver.next_ready().unwrap().unwrap();
    assert!(entry2.is_file());
    assert_eq!(entry2.name(), "testdir/file.txt");

    assert!(receiver.next_ready().unwrap().is_none());
    assert!(receiver.is_empty());
    assert_eq!(receiver.entries_read(), 2);
}

#[test]
fn incremental_receiver_handles_empty_list() {
    let protocol = ProtocolVersion::try_from(32u8).unwrap();
    let mut data = Vec::new();
    let writer = protocol::flist::FileListWriter::new(protocol);
    writer.write_end(&mut data, None).unwrap();

    let handshake = test_handshake();
    let config = test_config();
    let ctx = ReceiverContext::new_for_test(&handshake, config);

    let mut receiver = ctx.incremental_file_list_receiver(Cursor::new(&data[..]));

    assert!(receiver.next_ready().unwrap().is_none());
    assert!(receiver.is_empty());
    assert_eq!(receiver.entries_read(), 0);
}

#[test]
fn incremental_receiver_collect_sorted() {
    let protocol = ProtocolVersion::try_from(32u8).unwrap();
    let mut data = Vec::new();
    let mut writer = protocol::flist::FileListWriter::new(protocol);

    // Entries are supplied out of order to exercise the sort.
    let file1 = FileEntry::new_file("z_file.txt".into(), 50, 0o644);
    let file2 = FileEntry::new_file("a_file.txt".into(), 100, 0o644);
    let dir = FileEntry::new_directory("m_dir".into(), 0o755);

    writer.write_entry(&mut data, &file1).unwrap();
    writer.write_entry(&mut data, &file2).unwrap();
    writer.write_entry(&mut data, &dir).unwrap();
    writer.write_end(&mut data, None).unwrap();

    let handshake = test_handshake();
    let config = test_config();
    let ctx = ReceiverContext::new_for_test(&handshake, config);

    let receiver = ctx.incremental_file_list_receiver(Cursor::new(&data[..]));

    let entries = receiver.collect_sorted().unwrap();
    assert_eq!(entries.len(), 3);

    // Files should come before directories at the same level
    assert_eq!(entries[0].name(), "a_file.txt");
    assert_eq!(entries[1].name(), "z_file.txt");
    assert_eq!(entries[2].name(), "m_dir");
}

#[test]
fn incremental_receiver_iterator_interface() {
    let protocol = ProtocolVersion::try_from(32u8).unwrap();
    let mut data = Vec::new();
    let mut writer = protocol::flist::FileListWriter::new(protocol);

    let file = FileEntry::new_file("test.txt".into(), 100, 0o644);
    writer.write_entry(&mut data, &file).unwrap();
    writer.write_end(&mut data, None).unwrap();

    let handshake = test_handshake();
    let config = test_config();
    let ctx = ReceiverContext::new_for_test(&handshake, config);

    let receiver = ctx.incremental_file_list_receiver(Cursor::new(&data[..]));

    let entries: Vec<_> = receiver.collect::<Result<Vec<_>, _>>().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].name(), "test.txt");
}

#[test]
fn incremental_receiver_mark_directory_created() {
    let protocol = ProtocolVersion::try_from(32u8).unwrap();
    let mut data = Vec::new();
    let mut writer = protocol::flist::FileListWriter::new(protocol);

    // Add only a nested file (no directory entry)
    let file = FileEntry::new_file("existing/nested.txt".into(), 100, 0o644);
    writer.write_entry(&mut data, &file).unwrap();
    writer.write_end(&mut data, None).unwrap();

    let handshake = test_handshake();
    let config = test_config();
    let ctx = ReceiverContext::new_for_test(&handshake, config);

    let mut receiver = ctx.incremental_file_list_receiver(Cursor::new(&data[..]));

    // Mark the parent directory as already created
    receiver.mark_directory_created("existing");

    // Now the nested file should be immediately ready
    let entry = receiver.next_ready().unwrap().unwrap();
    assert_eq!(entry.name(), "existing/nested.txt");
}

#[test]
fn transfer_stats_has_incremental_fields() {
    let stats = TransferStats {
        files_listed: 0,
        num_dirs: 0,
        num_symlinks: 0,
        num_devices: 0,
        num_specials: 0,
        files_transferred: 0,
        transferred_file_size: 0,
        bytes_received: 0,
        bytes_sent: 0,
        total_source_bytes: 0,
        metadata_errors: vec![],
        io_error: 0,
        error_count: 0,
        entries_received: 100,
        directories_created: 10,
        directories_failed: 2,
        files_skipped: 5,
        delete_stats: DeleteStats::new(),
        created_stats: protocol::stats::CreatedStats::new(),
        delete_limit_exceeded: false,
        literal_data: 0,
        matched_data: 0,
        redo_count: 0,
        list_only_entries: vec![],
    };

    assert_eq!(stats.entries_received, 100);
    assert_eq!(stats.directories_created, 10);
    assert_eq!(stats.directories_failed, 2);
    assert_eq!(stats.files_skipped, 5);
}

#[test]
fn run_pipelined_incremental_compiles() {
    // This test just verifies the method signature is correct
    fn _check_signature<R: Read, W: Write + crate::writer::MsgInfoSender + ?Sized>(
        ctx: &mut ReceiverContext,
        reader: crate::reader::ServerReader<R>,
        writer: &mut W,
    ) {
        let _ = ctx.run_pipelined_incremental(reader, writer, PipelineConfig::default(), None);
    }
}

mod create_directory_incremental_tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn creates_directory_successfully() {
        let temp = TempDir::new().unwrap();
        let dest = temp.path();

        let entry = FileEntry::new_directory("subdir".into(), 0o755);
        let opts = metadata::MetadataOptions::default();
        let mut failed = FailedDirectories::new();

        let handshake = test_handshake();
        let config = test_config();
        let ctx = ReceiverContext::new_for_test(&handshake, config);

        let result = ctx.create_directory_incremental(
            dest,
            &entry,
            &opts,
            &mut failed,
            None,
            None,
            #[cfg(unix)]
            None,
        );

        assert!(result.is_ok());
        // Returns Some((true, _)) for a newly created dir.
        assert_eq!(result.unwrap().map(|(is_new, _)| is_new), Some(true));
        assert!(dest.join("subdir").exists());
        assert_eq!(failed.count(), 0);
    }

    /// Regression (exclude-lsh / upstream generator.c:1368-1383): with
    /// `--existing` (`ignore_non_existing`), a directory that is missing at
    /// the destination must NOT be created, and it must be marked failed so
    /// its descendants are skipped too. Before the fix, the remote-pull
    /// receiver created every directory from the file list unconditionally,
    /// so `--existing --include='*/' --exclude='*'` re-materialised empty
    /// directory skeletons that upstream leaves absent.
    #[test]
    fn existing_only_skips_missing_directory() {
        let temp = TempDir::new().unwrap();
        let dest = temp.path();

        let entry = FileEntry::new_directory("missing".into(), 0o755);
        let opts = metadata::MetadataOptions::default();
        let mut failed = FailedDirectories::new();

        let handshake = test_handshake();
        let mut config = test_config();
        config.file_selection.existing_only = true;
        let ctx = ReceiverContext::new_for_test(&handshake, config);

        let result = ctx.create_directory_incremental(
            dest,
            &entry,
            &opts,
            &mut failed,
            None,
            None,
            #[cfg(unix)]
            None,
        );

        assert!(result.is_ok());
        // Returns None (skipped) and the directory is never created on disk.
        assert_eq!(result.unwrap(), None);
        assert!(
            !dest.join("missing").exists(),
            "--existing must not create a missing directory on a remote pull"
        );
        // The skipped dir is marked failed so descendants are skipped via the
        // failed-ancestor check (mirrors upstream FLAG_MISSING_DIR).
        assert_eq!(failed.count(), 1);
    }

    /// With `--existing`, a directory that already exists at the destination
    /// is left in place (metadata still applies); only creation of *missing*
    /// directories is suppressed.
    #[test]
    fn existing_only_keeps_present_directory() {
        let temp = TempDir::new().unwrap();
        let dest = temp.path();
        std::fs::create_dir(dest.join("present")).unwrap();

        let entry = FileEntry::new_directory("present".into(), 0o755);
        let opts = metadata::MetadataOptions::default();
        let mut failed = FailedDirectories::new();

        let handshake = test_handshake();
        let mut config = test_config();
        config.file_selection.existing_only = true;
        let ctx = ReceiverContext::new_for_test(&handshake, config);

        let result = ctx.create_directory_incremental(
            dest,
            &entry,
            &opts,
            &mut failed,
            None,
            None,
            #[cfg(unix)]
            None,
        );

        assert!(result.is_ok());
        // Returns Some((false, _)): existing dir, not newly created.
        assert_eq!(result.unwrap().map(|(is_new, _)| is_new), Some(false));
        assert!(dest.join("present").exists());
        assert_eq!(failed.count(), 0);
    }

    /// An existing destination directory whose mtime differs from the sender
    /// entry must report `ITEM_REPORT_TIME`, so the transfer root `.` (and any
    /// existing directory) emits a `.d..t......` itemize row.
    ///
    /// WHY: upstream's `itemize()` (`generator.c:511-572`, reached from
    /// `generator.c:1480-1483` with `statret == 0`) sets `ITEM_REPORT_TIME`
    /// whenever `mtime_differs(&sxp->st, file)` for a directory under `--times`
    /// (`keep_time` true). This is independent of `--checksum`: a quick-check
    /// hunt surfaced it under `-c` because content-identical files are skipped
    /// while the root directory's mtime still differs. The prior receiver
    /// passed `iflags == 0` for every existing directory, so this row was never
    /// produced on a remote pull, diverging from upstream. The flags are
    /// computed against the pre-apply stat (before this call re-sets the
    /// directory mtime), matching upstream's itemize-before-set_file_attrs order.
    #[test]
    fn existing_directory_with_differing_mtime_reports_time_change() {
        use crate::generator::ItemFlags;

        let temp = TempDir::new().unwrap();
        let dest = temp.path();
        let present = dest.join("present");
        std::fs::create_dir(&present).unwrap();

        // Backdate the on-disk directory so its mtime differs from the entry.
        let old = filetime::FileTime::from_unix_time(1_500_000_000, 0);
        filetime::set_file_mtime(&present, old).unwrap();

        // Sender entry carries a newer mtime (2021-01-01 vs the 2017 on disk).
        let mut entry = FileEntry::new_directory("present".into(), 0o755);
        entry.set_mtime(1_609_459_200, 0);

        let opts = metadata::MetadataOptions::default();
        let mut failed = FailedDirectories::new();

        let handshake = test_handshake();
        let mut config = test_config();
        config.flags.times = true;
        let ctx = ReceiverContext::new_for_test(&handshake, config);

        let result = ctx
            .create_directory_incremental(
                dest,
                &entry,
                &opts,
                &mut failed,
                None,
                None,
                #[cfg(unix)]
                None,
            )
            .expect("create_directory_incremental succeeds");

        let (is_new, iflags) = result.expect("existing dir returns Some");
        assert!(!is_new, "the directory already existed");
        assert_ne!(
            iflags & ItemFlags::ITEM_REPORT_TIME,
            0,
            "a differing directory mtime must set ITEM_REPORT_TIME (upstream .d..t......)"
        );
    }

    /// The mirror-image gate: an existing directory whose mtime already matches
    /// the sender entry must NOT set `ITEM_REPORT_TIME`, so the significance
    /// gate drops the row exactly as upstream does when nothing changed.
    #[test]
    fn existing_directory_with_matching_mtime_reports_no_time_change() {
        use crate::generator::ItemFlags;

        let temp = TempDir::new().unwrap();
        let dest = temp.path();
        let present = dest.join("present");
        std::fs::create_dir(&present).unwrap();

        let when = filetime::FileTime::from_unix_time(1_609_459_200, 0);
        filetime::set_file_mtime(&present, when).unwrap();

        let mut entry = FileEntry::new_directory("present".into(), 0o755);
        entry.set_mtime(1_609_459_200, 0);

        let opts = metadata::MetadataOptions::default();
        let mut failed = FailedDirectories::new();

        let handshake = test_handshake();
        let mut config = test_config();
        config.flags.times = true;
        let ctx = ReceiverContext::new_for_test(&handshake, config);

        let (is_new, iflags) = ctx
            .create_directory_incremental(
                dest,
                &entry,
                &opts,
                &mut failed,
                None,
                None,
                #[cfg(unix)]
                None,
            )
            .expect("create_directory_incremental succeeds")
            .expect("existing dir returns Some");

        assert!(!is_new, "the directory already existed");
        assert_eq!(
            iflags & ItemFlags::ITEM_REPORT_TIME,
            0,
            "a matching directory mtime must not set ITEM_REPORT_TIME"
        );
    }

    #[test]
    fn skips_child_of_failed_parent() {
        let temp = TempDir::new().unwrap();
        let dest = temp.path();

        let entry = FileEntry::new_directory("failed_parent/child".into(), 0o755);
        let opts = metadata::MetadataOptions::default();
        let mut failed = FailedDirectories::new();
        failed.mark_failed("failed_parent");

        let handshake = test_handshake();
        let config = test_config();
        let ctx = ReceiverContext::new_for_test(&handshake, config);

        let result = ctx.create_directory_incremental(
            dest,
            &entry,
            &opts,
            &mut failed,
            None,
            None,
            #[cfg(unix)]
            None,
        );

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), None); // Returns None for skipped
        assert!(!dest.join("failed_parent/child").exists());
        assert_eq!(failed.count(), 2); // Parent + child marked as failed
    }

    /// A conflicting symlink at a directory-creation target must be replaced
    /// by a real directory, mirroring upstream `generator.c`.
    ///
    /// Upstream lstat-classifies the destination with `link_stat(fname,
    /// &sx.st, keep_dirlinks && is_dir)` (`generator.c:1356`). With
    /// `--keep-dirlinks` off, a symlink at the target (even a dangling one)
    /// lstats as `FT_SYMLINK != FT_DIR`, so upstream removes it via
    /// `delete_item(fname, ..., del_opts | DEL_FOR_DIR)` and then
    /// `do_mkdir_at()` (`generator.c:1451-1455`). The receiver's
    /// `classify_dir_destination` reproduces this: it removes the conflicting
    /// symlink and reports `ReplacedSymlink`, which drives a fresh `mkdir`.
    ///
    /// A dangling symlink is used deliberately: `Path::exists` follows it and
    /// reports "missing", which the old `.exists()` probe mistook for a plain
    /// new directory and let `mkdir` fail with `EEXIST` on the symlink. The
    /// lstat-based classifier must instead replace it and create a real
    /// directory.
    #[cfg(unix)]
    #[test]
    fn replaces_conflicting_symlink_with_real_directory() {
        use std::os::unix::fs::symlink;

        let temp = TempDir::new().unwrap();
        let dest = temp.path();

        // Plant a dangling symlink where the incoming directory will land.
        let leaf = dest.join("victim");
        symlink(dest.join("does-not-exist"), &leaf).expect("plant dangling symlink");

        let entry = FileEntry::new_directory("victim".into(), 0o755);
        let opts = metadata::MetadataOptions::default();
        let mut failed = FailedDirectories::new();

        let handshake = test_handshake();
        let config = test_config();
        let ctx = ReceiverContext::new_for_test(&handshake, config);

        let result = ctx.create_directory_incremental(
            dest,
            &entry,
            &opts,
            &mut failed,
            None,
            None,
            #[cfg(unix)]
            None,
        );

        // The conflicting symlink is removed and a real directory is created:
        // upstream reports `FLAG_DIR_CREATED`, so this is a freshly made dir.
        assert_eq!(
            result
                .expect("classifier replaces the symlink and mkdirs a real directory")
                .map(|(is_new, _)| is_new),
            Some(true),
            "a conflicting symlink at a dir target must be replaced, not skipped"
        );
        let meta = std::fs::symlink_metadata(&leaf).expect("target must exist");
        assert!(
            meta.file_type().is_dir(),
            "the destination must be a real directory, not the original symlink"
        );
        assert_eq!(failed.count(), 0, "replacement is not a failure");
    }

    /// Fail-loud invariant (Rule 12): a genuine non-EACCES error from the
    /// underlying `mkdir` must propagate as `Err`, never be coerced to
    /// `Ok(None)` / `mark_failed`, which would hide it from the caller's
    /// exit-code path.
    ///
    /// EACCES is the only non-fatal `mkdir` class (upstream
    /// `receiver.c:693-700` folds it into `io_error` and continues); every
    /// other errno is a hard failure. This shapes an `ENOTDIR` by placing a
    /// regular file in the middle of the directory path (`afile/sub`, where
    /// `afile` is a file), so `mkdir` on `afile/sub` fails because a path
    /// component is not a directory. That error class is neither `NotFound`
    /// (which would trigger the `create_dir_all` parent-walk) nor
    /// `PermissionDenied` (the non-fatal branch), so it must surface as `Err`.
    #[cfg(unix)]
    #[test]
    fn surfaces_non_permission_error_from_mkdir() {
        let temp = TempDir::new().unwrap();
        let dest = temp.path();

        // A regular file where a parent directory is expected forces ENOTDIR
        // when the receiver tries to create `afile/sub` beneath it.
        std::fs::write(dest.join("afile"), b"not a directory").expect("plant regular file");

        let entry = FileEntry::new_directory("afile/sub".into(), 0o755);
        let opts = metadata::MetadataOptions::default();
        let mut failed = FailedDirectories::new();

        let handshake = test_handshake();
        let config = test_config();
        let ctx = ReceiverContext::new_for_test(&handshake, config);

        let result = ctx.create_directory_incremental(
            dest,
            &entry,
            &opts,
            &mut failed,
            None,
            None,
            #[cfg(unix)]
            None,
        );

        // Fail loud: the underlying ENOTDIR must propagate. Coercing it to
        // `Ok(None)` would hide the failure from the caller's exit-code path.
        let err = result
            .expect_err("non-EACCES mkdir failure must propagate as Err, not be coerced to Ok");
        assert_ne!(
            err.kind(),
            std::io::ErrorKind::PermissionDenied,
            "EACCES takes the non-fatal branch; this scenario is shaped to avoid it"
        );
        assert_ne!(
            err.kind(),
            std::io::ErrorKind::NotFound,
            "NotFound would trigger the create_dir_all parent-walk, not a hard error"
        );
    }

    /// Upstream-parity guard for `receiver.c:693-700`: a
    /// `PermissionDenied` error on mkdir is non-fatal. The receiver
    /// must mark the directory as failed and return `Ok(None)` so the
    /// rest of the transfer can proceed (mirrors upstream's
    /// `io_error |= IOERR_GENERAL` accumulator).
    #[cfg(unix)]
    #[test]
    fn treats_permission_denied_as_non_fatal_skip() {
        use std::os::unix::fs::PermissionsExt;

        let temp = TempDir::new().unwrap();
        let dest = temp.path();

        // Strip write permission from the destination so the kernel
        // rejects mkdir with EACCES. Root bypasses DAC so the mkdir
        // would succeed; detect that case by re-checking the error
        // class after the call and skipping the assertion when the
        // environment is not amenable.
        let mut perms = std::fs::metadata(dest).unwrap().permissions();
        let original_mode = perms.mode();
        perms.set_mode(0o500);
        std::fs::set_permissions(dest, perms).expect("chmod 0500 dest");

        let entry = FileEntry::new_directory("subdir".into(), 0o755);
        let opts = metadata::MetadataOptions::default();
        let mut failed = FailedDirectories::new();

        let handshake = test_handshake();
        let config = test_config();
        let ctx = ReceiverContext::new_for_test(&handshake, config);

        let result = ctx.create_directory_incremental(
            dest,
            &entry,
            &opts,
            &mut failed,
            None,
            None,
            #[cfg(unix)]
            None,
        );

        // Restore permissions before assertions so TempDir cleanup
        // works regardless of the outcome.
        let mut perms = std::fs::metadata(dest).unwrap().permissions();
        perms.set_mode(original_mode);
        std::fs::set_permissions(dest, perms).expect("restore dest perms");

        // Root (or a non-DAC-respecting filesystem) bypasses chmod so
        // mkdir succeeds. Only enforce the upstream-parity branch when
        // the kernel actually produced EACCES.
        let succeeded_under_root = matches!(&result, Ok(Some((true, _))));
        if succeeded_under_root {
            return;
        }

        let value = result.expect("EACCES must remain non-fatal per upstream receiver.c:693-700");
        assert_eq!(
            value, None,
            "EACCES must produce Ok(None) so the receiver continues with the rest of the transfer"
        );
        assert_eq!(
            failed.count(),
            1,
            "the failed directory must be recorded so descendants are skipped"
        );
    }
}

#[cfg(feature = "incremental-flist")]
mod incremental_mode_tests {
    use super::super::super::support::PHASE1_CHECKSUM_LENGTH;
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn failed_directories_skips_nested_children() {
        let mut failed = FailedDirectories::new();
        failed.mark_failed("a/b");

        // Direct child
        assert!(failed.failed_ancestor("a/b/file.txt").is_some());
        // Nested child
        assert!(failed.failed_ancestor("a/b/c/d/file.txt").is_some());
        // Sibling - not affected
        assert!(failed.failed_ancestor("a/c/file.txt").is_none());
        // Parent - not affected
        assert!(failed.failed_ancestor("a/file.txt").is_none());
    }

    #[test]
    fn failed_directories_handles_root_level() {
        let mut failed = FailedDirectories::new();
        failed.mark_failed("toplevel");

        assert!(failed.failed_ancestor("toplevel/sub/file.txt").is_some());
        assert!(failed.failed_ancestor("other/file.txt").is_none());
    }

    #[test]
    fn stats_tracks_incremental_fields() {
        let stats = TransferStats {
            entries_received: 100,
            directories_created: 20,
            directories_failed: 2,
            files_skipped: 10,
            files_transferred: 68,
            ..Default::default()
        };

        // Verify consistency
        assert_eq!(
            stats.directories_created + stats.directories_failed,
            22 // total directories
        );
    }

    #[test]
    fn create_directory_incremental_nested() {
        let temp = TempDir::new().unwrap();
        let dest = temp.path();

        let entry = FileEntry::new_directory("a/b/c".into(), 0o755);
        let opts = metadata::MetadataOptions::default();
        let mut failed = FailedDirectories::new();

        let handshake = test_handshake();
        let config = test_config();
        let ctx = ReceiverContext::new_for_test(&handshake, config);

        let result = ctx.create_directory_incremental(
            dest,
            &entry,
            &opts,
            &mut failed,
            None,
            None,
            #[cfg(unix)]
            None,
        );

        assert!(result.is_ok());
        assert_eq!(result.unwrap().map(|(is_new, _)| is_new), Some(true));
        assert!(dest.join("a/b/c").exists());
    }

    #[test]
    fn failed_directories_propagates_to_deeply_nested() {
        let mut failed = FailedDirectories::new();
        failed.mark_failed("level1");

        // All descendants should be affected
        assert!(failed.failed_ancestor("level1/level2").is_some());
        assert!(failed.failed_ancestor("level1/level2/level3").is_some());
        assert!(
            failed
                .failed_ancestor("level1/level2/level3/file.txt")
                .is_some()
        );
    }

    #[test]
    fn checksum_length_phase1_equals_short_sum_length() {
        assert_eq!(
            PHASE1_CHECKSUM_LENGTH.get(),
            signature::block_size::SHORT_SUM_LENGTH,
        );
        assert_eq!(PHASE1_CHECKSUM_LENGTH.get(), 2);
    }

    #[test]
    fn checksum_length_redo_equals_max_sum_length() {
        assert_eq!(
            super::super::super::super::REDO_CHECKSUM_LENGTH.get(),
            signature::block_size::MAX_SUM_LENGTH,
        );
        assert_eq!(super::super::super::super::REDO_CHECKSUM_LENGTH.get(), 16);
    }

    #[test]
    fn checksum_length_phase1_less_than_redo() {
        assert!(PHASE1_CHECKSUM_LENGTH < super::super::super::super::REDO_CHECKSUM_LENGTH);
    }

    #[test]
    fn transfer_stats_default_values() {
        let stats = TransferStats::default();

        assert_eq!(stats.entries_received, 0);
        assert_eq!(stats.directories_created, 0);
        assert_eq!(stats.directories_failed, 0);
        assert_eq!(stats.files_skipped, 0);
        assert_eq!(stats.files_transferred, 0);
        assert_eq!(stats.bytes_received, 0);
    }

    /// URV-6.b regression: when the incremental driver is configured with
    /// `--delete` and the destination contains extraneous entries, the
    /// receiver must run `delete_extraneous_files` and surface non-zero
    /// counters so the goodbye phase can emit `NDX_DEL_STATS`.
    ///
    /// Mirrors the delete-pass call site added in `run_pipelined_incremental`
    /// (matching the existing wiring in `run_pipelined`). Prior to URV-6.b
    /// the incremental driver skipped the sweep entirely so `DeleteStats`
    /// stayed zero in every default-feature build.
    ///
    /// upstream: generator.c:do_delete_pass()
    #[test]
    fn incremental_driver_populates_delete_stats() {
        use std::ffi::OsString;

        use super::super::super::support::TestDeletionWriter;

        let temp = TempDir::new().unwrap();
        let dest = temp.path();

        // Destination has two extraneous files not in the sender's flist.
        std::fs::write(dest.join("stale_a.txt"), b"old").unwrap();
        std::fs::write(dest.join("stale_b.txt"), b"old").unwrap();
        std::fs::write(dest.join("keep.txt"), b"keep").unwrap();

        let handshake = test_handshake();
        let mut config = test_config();
        config.flags.delete = true;
        config.args = vec![OsString::from(dest.to_str().unwrap())];
        let mut ctx = ReceiverContext::new_for_test(&handshake, config);

        // Sender's flist: "." plus the single kept file.
        ctx.file_list
            .push(FileEntry::new_directory(".".into(), 0o755));
        ctx.file_list
            .push(FileEntry::new_file("keep.txt".into(), 4, 0o644));

        // Call the delete pass the same way `run_pipelined_incremental` does.
        let mut writer = TestDeletionWriter;
        let (delete_stats, delete_limit_exceeded, _io_error_bits) = ctx
            .delete_extraneous_files(
                dest,
                #[cfg(unix)]
                None,
                &mut writer,
            )
            .unwrap();

        // Mirror the field assignment performed at the end of
        // `run_pipelined_incremental`.
        let stats = TransferStats {
            delete_stats,
            delete_limit_exceeded,
            ..Default::default()
        };

        assert_eq!(
            stats.delete_stats.files, 2,
            "extraneous files should populate delete_stats.files",
        );
        assert!(
            stats.delete_stats.total() >= 2,
            "delete_stats.total() must reflect the swept extraneous entries",
        );
        assert!(!stats.delete_limit_exceeded);
        assert!(!dest.join("stale_a.txt").exists());
        assert!(!dest.join("stale_b.txt").exists());
        assert!(dest.join("keep.txt").exists());
    }

    /// EDG-SANDBOX.A regression: the parallel `read_dir` worker in
    /// `delete_extraneous_files` must surface a non-EACCES/non-NotFound
    /// scan failure through the `io_err_bits` slot of the return tuple
    /// so the receiver's overall `io_error` field drives a non-zero
    /// `RERR_PARTIAL=23` exit instead of silently skipping the subtree.
    ///
    /// Before the fix, planting a regular file where the receiver's
    /// file list expected a directory caused the worker to discard the
    /// `ENOTDIR` returned by `read_dir`, return empty stats, and exit
    /// `rc=0` with the deletions in that subtree silently skipped.
    /// The fix discriminates EACCES (upstream-parity non-fatal, matches
    /// `generator.c:delete_in_dir`) from the ELOOP/EOPNOTSUPP/ENOTDIR
    /// class, which must OR `IOERR_GENERAL` into the third tuple slot
    /// so the caller's `stats.io_error |= io_bits` accumulation in
    /// `pipelined.rs` / `pipelined_incremental.rs` produces the
    /// upstream-parity non-zero exit.
    ///
    /// upstream: generator.c:delete_in_dir() - "opendir failed" classifies
    /// EACCES as non-fatal (io_error bit only) and every other class as a
    /// fatal scan failure.
    #[cfg(unix)]
    #[test]
    fn delete_extraneous_files_surfaces_non_eacces_scan_error() {
        use std::ffi::OsString;

        use super::super::super::support::TestDeletionWriter;
        use crate::generator::io_error_flags::IOERR_GENERAL;

        let temp = TempDir::new().unwrap();
        let dest = temp.path();

        // Plant a regular file at the path the sender's file list claims
        // is a directory. `read_dir(dest/subdir)` returns `ENOTDIR`
        // (mapped to `ErrorKind::NotADirectory` on Rust >= 1.83), which
        // is the fail-loud class - not the upstream-parity EACCES branch.
        std::fs::write(dest.join("subdir"), b"not a directory").unwrap();

        let handshake = test_handshake();
        let mut config = test_config();
        config.flags.delete = true;
        config.args = vec![OsString::from(dest.to_str().unwrap())];
        let mut ctx = ReceiverContext::new_for_test(&handshake, config);

        // Sender's flist references `subdir/child.txt`, so the worker
        // map keys `subdir` as a scan target.
        ctx.file_list
            .push(FileEntry::new_directory(".".into(), 0o755));
        ctx.file_list
            .push(FileEntry::new_directory("subdir".into(), 0o755));
        ctx.file_list
            .push(FileEntry::new_file("subdir/child.txt".into(), 4, 0o644));

        let mut writer = TestDeletionWriter;
        let (_stats, _limit_exceeded, io_bits) = ctx
            .delete_extraneous_files(
                dest,
                #[cfg(unix)]
                None,
                &mut writer,
            )
            .expect(
                "delete_extraneous_files must return Ok and surface fail-loud \
                 scan errors via the io_err_bits tuple slot",
            );

        assert_ne!(
            io_bits & IOERR_GENERAL,
            0,
            "non-EACCES scan error must set IOERR_GENERAL in io_err_bits so \
             the receiver's stats.io_error drives a non-zero exit",
        );
    }
}

/// #517: the `*deleting` itemize stream must be deterministic and match
/// upstream `generator.c:delete_in_dir()` order across runs. Before the
/// fix, the scan set derived from `HashMap::keys()` (hash-randomized per
/// process), so a multi-directory `--delete` emitted its `*deleting`
/// lines in an arbitrary order that varied run to run. The fix orders
/// the emission by parent directory ascending, entries descending -
/// upstream's observable order.
///
/// This runs the delete pass twice against freshly rebuilt destinations
/// and asserts an identical, upstream-sorted line sequence both times.
///
/// Deletion itemize ordering is feature-independent, so this lives at the
/// file's top level (only `#[cfg(unix)]`, matching `CapturingDeletionWriter`)
/// rather than inside the `incremental-flist`-gated submodule - it must run
/// in every feature combo, including the `--no-default-features` musl cell.
///
/// upstream: generator.c:delete_in_dir() - sorted dirlist, reverse iter
#[cfg(unix)]
#[test]
fn delete_itemize_order_is_deterministic_and_upstream_sorted() {
    use std::ffi::OsString;

    use tempfile::TempDir;

    use super::super::support::CapturingDeletionWriter;

    // Build a multi-directory destination with extraneous entries in the
    // root and in two kept subdirectories. Ordering must not depend on
    // read_dir or HashMap iteration order, so exercise many names.
    let run = || -> Vec<String> {
        let temp = TempDir::new().unwrap();
        let dest = temp.path();

        // Kept directories (present in the sender's flist).
        std::fs::create_dir(dest.join("alpha")).unwrap();
        std::fs::create_dir(dest.join("beta")).unwrap();
        std::fs::write(dest.join("alpha").join("keep.txt"), b"k").unwrap();
        std::fs::write(dest.join("beta").join("keep.txt"), b"k").unwrap();

        // Extraneous entries: root files, and files in each kept dir.
        for name in ["r_z.txt", "r_a.txt", "r_m.txt"] {
            std::fs::write(dest.join(name), b"x").unwrap();
        }
        for name in ["e_z.txt", "e_a.txt", "e_m.txt"] {
            std::fs::write(dest.join("alpha").join(name), b"x").unwrap();
            std::fs::write(dest.join("beta").join(name), b"x").unwrap();
        }

        let handshake = test_handshake();
        let mut config = test_config();
        config.flags.delete = true;
        config.flags.info_flags.itemize = true;
        config.args = vec![OsString::from(dest.to_str().unwrap())];
        let mut ctx = ReceiverContext::new_for_test(&handshake, config);

        ctx.file_list
            .push(FileEntry::new_directory(".".into(), 0o755));
        ctx.file_list
            .push(FileEntry::new_directory("alpha".into(), 0o755));
        ctx.file_list
            .push(FileEntry::new_directory("beta".into(), 0o755));
        ctx.file_list
            .push(FileEntry::new_file("alpha/keep.txt".into(), 1, 0o644));
        ctx.file_list
            .push(FileEntry::new_file("beta/keep.txt".into(), 1, 0o644));

        let mut writer = CapturingDeletionWriter::default();
        ctx.delete_extraneous_files(dest, None, &mut writer)
            .unwrap();
        writer.lines
    };

    // Directories ascending ("." < "alpha" < "beta"); within each,
    // entries descending. Verified against `rsync 3.4.4 -rii --delete`.
    let expected = vec![
        "*deleting   r_z.txt".to_owned(),
        "*deleting   r_m.txt".to_owned(),
        "*deleting   r_a.txt".to_owned(),
        "*deleting   alpha/e_z.txt".to_owned(),
        "*deleting   alpha/e_m.txt".to_owned(),
        "*deleting   alpha/e_a.txt".to_owned(),
        "*deleting   beta/e_z.txt".to_owned(),
        "*deleting   beta/e_m.txt".to_owned(),
        "*deleting   beta/e_a.txt".to_owned(),
    ];

    let first = run();
    let second = run();
    assert_eq!(first, second, "delete itemize order must be deterministic");
    assert_eq!(
        first, expected,
        "delete itemize order must match upstream delete_in_dir() order",
    );
}
