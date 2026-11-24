//! The disk thread's `one_inplace` output: the partial-dir basis rewritten in
//! place, sized to the received length and renamed onto the destination.
//!
//! upstream: receiver.c:1153-1155 (`one_inplace`), :1212-1244 (the output is
//! `partialptr`, opened through `secure_recv_open(..., one_inplace)`), :668-671
//! (`inplace_sizing` truncation) and :1302-1335 (the commit and the failure
//! arms).

use std::fs;
use std::os::unix::fs::{MetadataExt, symlink};
use std::path::{Path, PathBuf};

use crate::delta_apply::ChecksumVerifier;
use crate::pipeline::messages::{BeginMessage, ExpectedChecksum, FileMessage};
use crate::temp_guard::{CommitOp, commit_op_failure};

use super::config::{DiskCommitConfig, PartialMode};
use super::thread::spawn_disk_thread;

fn md5(data: &[u8]) -> ExpectedChecksum {
    let mut v = ChecksumVerifier::for_algorithm(protocol::ChecksumAlgorithm::MD5);
    v.update(data);
    let mut bytes = [0u8; ChecksumVerifier::MAX_DIGEST_LEN];
    let len = v.finalize_into(&mut bytes);
    ExpectedChecksum { bytes, len }
}

fn config() -> DiskCommitConfig {
    DiskCommitConfig {
        partial_mode: PartialMode::PartialDir(PathBuf::from("pd")),
        ..DiskCommitConfig::default()
    }
}

fn begin(dest: &Path, partial: &Path, target_size: u64) -> FileMessage {
    FileMessage::Begin(Box::new(BeginMessage {
        file_path: dest.to_path_buf(),
        target_size,
        file_entry_index: 0,
        checksum_verifier: Some(ChecksumVerifier::for_algorithm(
            protocol::ChecksumAlgorithm::MD5,
        )),
        is_device_target: false,
        is_inplace: false,
        one_inplace_target: Some(partial.to_path_buf()),
        append_offset: 0,
        xattr_list: None,
        xattr_basis: Some(partial.to_path_buf()),
        dest_mode_basis: None,
        file_entry: None,
        log_name: None,
    }))
}

/// Sends a one_inplace update that keeps the partial file's first block,
/// rewrites its second and ends there, then returns the commit result.
fn run(dest: &Path, partial: &Path, expected: ExpectedChecksum) -> std::io::Result<()> {
    let h = spawn_disk_thread(config()).unwrap();
    h.file_tx.send(begin(dest, partial, 16)).unwrap();
    h.file_tx
        .send(FileMessage::SkipMatched(b"AAAAAAAA".to_vec()))
        .unwrap();
    h.file_tx
        .send(FileMessage::Chunk(b"CCCCCCCC".to_vec()))
        .unwrap();
    h.file_tx
        .send(FileMessage::Commit {
            expected_checksum: expected,
        })
        .unwrap();
    let result = h.result_rx.recv().unwrap().map(drop);
    h.file_tx.send(FileMessage::Shutdown).unwrap();
    h.join_handle.join().unwrap();
    result
}

/// WHY: upstream writes a partial-dir resume into the partial file itself and
/// renames it onto the destination; the new data may be shorter than the
/// partial file, which `inplace_sizing` cuts back. A temp+rename receiver
/// gives the destination a new inode; one that skips the cut leaves the
/// partial file's stale tail on it.
#[test]
fn one_inplace_rewrites_the_partial_file_and_renames_it_onto_the_destination() {
    let _registry_lock = test_support::cleanup_registry_test_guard();
    let dir = test_support::create_tempdir();
    let dest = dir.path().join("f");
    let partial = dir.path().join("pd/f");
    fs::create_dir(dir.path().join("pd")).unwrap();
    fs::write(&partial, b"AAAAAAAABBBBBBBBstale-tail").unwrap();
    let inode = fs::metadata(&partial).unwrap().ino();

    run(&dest, &partial, md5(b"AAAAAAAACCCCCCCC")).expect("commit");

    assert_eq!(fs::read(&dest).unwrap(), b"AAAAAAAACCCCCCCC");
    assert_eq!(fs::metadata(&dest).unwrap().ino(), inode);
    assert!(!partial.exists(), "the partial file must be renamed away");
}

/// WHY: the partial-dir path is opened through the ownership walk, which never
/// follows a symlink at the leaf - even one the receiver owns
/// (receiver.c:131-150 opens the leaf `O_NOFOLLOW`). Opening it by path would
/// write the received data through the link.
#[test]
fn one_inplace_refuses_a_symlink_at_the_partial_file() {
    let _registry_lock = test_support::cleanup_registry_test_guard();
    let dir = test_support::create_tempdir();
    let dest = dir.path().join("f");
    let partial = dir.path().join("pd/f");
    let victim = dir.path().join("victim");
    fs::create_dir(dir.path().join("pd")).unwrap();
    fs::write(&victim, b"AAAAAAAABBBBBBBB").unwrap();
    symlink(&victim, &partial).unwrap();

    let error = run(&dest, &partial, md5(b"AAAAAAAACCCCCCCC")).expect_err("leaf symlink");

    let (op, path) = commit_op_failure(&error).expect("tagged commit failure");
    assert!(matches!(op, CommitOp::Open), "{error}");
    assert_eq!(
        path,
        partial.as_path(),
        "upstream names fnametmp: the partial file"
    );
    assert_eq!(fs::read(&victim).unwrap(), b"AAAAAAAABBBBBBBB");
    assert!(!dest.exists());
}

/// WHY: a one_inplace update that fails verification stays in the partial
/// file it was written in, at its received length, and the destination is
/// left alone (receiver.c:1318-1335 neither keeps nor unlinks it).
#[test]
fn one_inplace_verification_failure_leaves_the_update_in_the_partial_file() {
    let _registry_lock = test_support::cleanup_registry_test_guard();
    let dir = test_support::create_tempdir();
    let dest = dir.path().join("f");
    let partial = dir.path().join("pd/f");
    fs::create_dir(dir.path().join("pd")).unwrap();
    fs::write(&partial, b"AAAAAAAABBBBBBBBstale-tail").unwrap();
    let inode = fs::metadata(&partial).unwrap().ino();

    run(&dest, &partial, md5(b"the sender's other bytes"))
        .expect("a failed verify is not an error");

    assert!(
        !dest.exists(),
        "a failed verification must not reach the destination"
    );
    assert_eq!(fs::read(&partial).unwrap(), b"AAAAAAAACCCCCCCC");
    assert_eq!(fs::metadata(&partial).unwrap().ino(), inode);
}
