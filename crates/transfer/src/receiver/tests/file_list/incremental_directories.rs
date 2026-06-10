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
    // Create test data with a simple file list
    let protocol = ProtocolVersion::try_from(32u8).unwrap();
    let mut data = Vec::new();
    let mut writer = protocol::flist::FileListWriter::new(protocol);

    // Add a directory and a file
    let dir = FileEntry::new_directory("testdir".into(), 0o755);
    let file = FileEntry::new_file("testdir/file.txt".into(), 100, 0o644);

    writer.write_entry(&mut data, &dir).unwrap();
    writer.write_entry(&mut data, &file).unwrap();
    writer.write_end(&mut data, None).unwrap();

    // Create handshake and config
    let handshake = test_handshake();
    let config = test_config();
    let ctx = ReceiverContext::new_for_test(&handshake, config);

    // Create incremental receiver
    let mut receiver = ctx.incremental_file_list_receiver(Cursor::new(&data[..]));

    // First entry should be the directory (it has no parent dependency)
    let entry1 = receiver.next_ready().unwrap().unwrap();
    assert!(entry1.is_dir());
    assert_eq!(entry1.name(), "testdir");

    // Second entry should be the file (parent dir now exists)
    let entry2 = receiver.next_ready().unwrap().unwrap();
    assert!(entry2.is_file());
    assert_eq!(entry2.name(), "testdir/file.txt");

    // No more entries
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

    // Add entries in random order
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

    // collect_sorted should return entries in sorted order
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

    // Use iterator interface
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
        files_transferred: 0,
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
        delete_limit_exceeded: false,
        literal_data: 0,
        matched_data: 0,
        redo_count: 0,
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
            #[cfg(unix)]
            None,
        );

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), Some(true)); // Returns Some(true) for new dir
        assert!(dest.join("subdir").exists());
        assert_eq!(failed.count(), 0);
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
            #[cfg(unix)]
            None,
        );

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), None); // Returns None for skipped
        assert!(!dest.join("failed_parent/child").exists());
        assert_eq!(failed.count(), 2); // Parent + child marked as failed
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

        // Create nested directory
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
            #[cfg(unix)]
            None,
        );

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), Some(true));
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
        let (delete_stats, delete_limit_exceeded) = ctx
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
    /// `delete_extraneous_files` must propagate a non-EACCES/non-NotFound
    /// scan failure to the outer caller instead of silently coercing it to
    /// `(DeleteStats::new(), Vec::new())`.
    ///
    /// Before this fix, planting a regular file where the receiver's
    /// file list expected a directory caused the worker to discard the
    /// `ENOTDIR` returned by `read_dir`, return empty stats, and exit
    /// `rc=0` with the deletions in that subtree silently skipped. The
    /// fix discriminates EACCES (upstream-parity non-fatal, matches
    /// `generator.c:delete_in_dir`) from the ELOOP/EOPNOTSUPP/ENOTDIR
    /// class which must fail loud.
    ///
    /// upstream: generator.c:delete_in_dir() - "opendir failed" classifies
    /// EACCES as non-fatal (io_error bit only) and every other class as a
    /// fatal scan failure.
    #[cfg(unix)]
    #[test]
    fn delete_extraneous_files_surfaces_non_eacces_scan_error() {
        use std::ffi::OsString;

        use super::super::super::support::TestDeletionWriter;

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
        let err = ctx
            .delete_extraneous_files(
                dest,
                #[cfg(unix)]
                None,
                &mut writer,
            )
            .expect_err(
                "non-EACCES scan error must propagate as Err, not be coerced to empty stats",
            );

        assert_ne!(
            err.kind(),
            std::io::ErrorKind::PermissionDenied,
            "EACCES is the upstream-parity non-fatal branch; this scenario \
             plants ENOTDIR to exercise the fail-loud branch",
        );
        assert_ne!(
            err.kind(),
            std::io::ErrorKind::NotFound,
            "NotFound is the upstream-parity continue-on-vanished branch",
        );
    }
}
