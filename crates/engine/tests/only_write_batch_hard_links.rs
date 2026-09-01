//! Regression test for UTS `batch-only-remove-source-regression`: a batch
//! written with `-H` must carry the hardlink clusters, and replaying it must
//! rebuild them.
//!
//! Before the fix the two ends failed independently. The writer's
//! `build_protocol_file_entry()` never set any hardlink identity, so the
//! encoded flist carried neither `XMIT_HLINKED` nor a group index and the
//! payload was repeated once per cluster member; replay decoded the hardlink
//! fields an upstream-written batch does carry but never acted on them. A
//! cluster therefore came back as unrelated copies whichever side produced the
//! batch.
//!
//! # Upstream Reference
//!
//! - `flist.c:599-625` - `send_file_entry()` flags the first sighting of an
//!   inode `XMIT_HLINK_FIRST` and every repeat a follower.
//! - `flist.c:668-672` - the follower entry is just flags, name and
//!   `write_varint(first_hlink_ndx)`.
//! - `flist.c:1335-1341` - `recv_file_entry()` stamps `F_HL_GNUM` in wire
//!   order.
//! - `hlink.c:113-194` - `match_gnums()` clusters by that tag so one member is
//!   transferred and the rest are linked to it.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::MetadataExt;
use std::sync::{Arc, Mutex};

use batch::{BatchConfig, BatchFlags, BatchMode, BatchReader, BatchWriter};
use engine::local_copy::{LocalCopyExecution, LocalCopyOptions, LocalCopyPlan};
use protocol::CompatibilityFlags;
use tempfile::tempdir;

/// Builds an `--only-write-batch` writer whose recorded stream flags say
/// `-rH`, since the reader and the flist encoder both configure themselves
/// from the header rather than from the live options.
///
/// The compat flags mirror the production set assembled by
/// `cli::frontend::execution::drive::workflow::run`, `INC_RECURSE` deliberately
/// omitted so the flist encodes flat.
fn make_writer(path: &std::path::Path) -> Arc<Mutex<BatchWriter>> {
    let compat_flags = CompatibilityFlags::SAFE_FILE_LIST
        | CompatibilityFlags::AVOID_XATTR_OPTIMIZATION
        | CompatibilityFlags::CHECKSUM_SEED_FIX
        | CompatibilityFlags::INPLACE_PARTIAL_DIR
        | CompatibilityFlags::VARINT_FLIST_FLAGS;
    let config = BatchConfig::new(
        BatchMode::OnlyWrite,
        path.to_string_lossy().into_owned(),
        32,
    )
    .with_compat_flags(compat_flags.bits() as i32)
    .with_checksum_seed(1);
    let mut writer = BatchWriter::new(config).expect("create batch writer");
    let flags = BatchFlags {
        recurse: true,
        preserve_hard_links: true,
        ..Default::default()
    };
    writer.write_header(flags).expect("write batch header");
    Arc::new(Mutex::new(writer))
}

/// A three-member hardlink cluster plus an unrelated file of identical content
/// must survive `--only-write-batch` followed by `--read-batch`: every member
/// materialises with the source payload, the three share one inode, and the
/// look-alike that was never hard-linked keeps an inode of its own.
#[test]
fn only_write_batch_replay_rebuilds_hard_link_clusters() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("src");
    let dest_write = temp.path().join("dst_write");
    let dest_replay = temp.path().join("dst_replay");
    let batch_path = temp.path().join("batch.bin");

    fs::create_dir_all(&source).expect("create source dir");
    fs::create_dir_all(&dest_write).expect("create write-side dest");
    fs::create_dir_all(&dest_replay).expect("create replay-side dest");

    let payload: Vec<u8> = (0..40 * 1024).map(|i| (i % 251) as u8).collect();
    // "b-leader" sorts before "m-middle" and "z-trailer", so the cluster
    // leader is not the last member the walk reaches; the sort mapping has to
    // be what picks which member carries the payload.
    fs::write(source.join("b-leader"), &payload).expect("write cluster leader");
    fs::hard_link(source.join("b-leader"), source.join("m-middle")).expect("link second member");
    fs::hard_link(source.join("b-leader"), source.join("z-trailer")).expect("link third member");
    // Same bytes, separate inode: proves the grouping keys on (dev, ino) and
    // not on content.
    fs::write(source.join("a-twin"), &payload).expect("write unlinked twin");

    // A pre-existing destination whose content differs, so a member that is
    // neither transferred nor linked keeps visibly stale bytes.
    for name in ["a-twin", "b-leader", "m-middle", "z-trailer"] {
        fs::write(dest_replay.join(name), b"stale destination bytes").expect("seed destination");
    }

    let writer = make_writer(&batch_path);
    let options = LocalCopyOptions::default()
        .recursive(true)
        .hard_links(true)
        .batch_writer(Some(Arc::clone(&writer)));

    let mut src_os = source.clone().into_os_string();
    src_os.push("/");
    let operands = vec![src_os, dest_write.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    plan.execute_with_options(LocalCopyExecution::DryRun, options)
        .expect("only-write-batch dry-run succeeds");

    Arc::try_unwrap(writer)
        .expect("writer uniquely owned")
        .into_inner()
        .expect("writer mutex not poisoned")
        .finalize()
        .expect("finalize batch writer");

    let read_cfg = BatchConfig::new(
        BatchMode::Read,
        batch_path.to_string_lossy().into_owned(),
        32,
    )
    .with_active_flags(BatchFlags {
        recurse: true,
        preserve_hard_links: true,
        ..Default::default()
    });

    // The flist itself must name the cluster: one member flagged first and the
    // other two carrying its index.
    let mut reader = BatchReader::new(read_cfg.clone()).expect("open batch reader");
    reader.read_header().expect("read batch header");
    let entries = reader.read_protocol_flist().expect("decode flist");
    let cluster: Vec<&protocol::flist::FileEntry> = entries
        .iter()
        .filter(|entry| {
            matches!(entry.name(), "b-leader" | "m-middle" | "z-trailer") && entry.hlinked()
        })
        .collect();
    assert_eq!(
        cluster.len(),
        3,
        "all three cluster members must be flagged XMIT_HLINKED; got {:?}",
        entries
            .iter()
            .map(|e| (e.name().to_owned(), e.hlinked()))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        cluster.iter().filter(|entry| entry.hlink_first()).count(),
        1,
        "exactly one member carries XMIT_HLINK_FIRST"
    );
    let twin = entries
        .iter()
        .find(|entry| entry.name() == "a-twin")
        .expect("unlinked twin present in flist");
    assert!(
        !twin.hlinked(),
        "a file with st_nlink == 1 must not be flagged as hard-linked"
    );
    drop(reader);

    batch::replay::replay(&read_cfg, &dest_replay, 0).expect("replay succeeds");

    for name in ["a-twin", "b-leader", "m-middle", "z-trailer"] {
        assert_eq!(
            fs::read(dest_replay.join(name)).expect("read replayed file"),
            payload,
            "{name} must carry the source payload after replay"
        );
    }

    let ino = |name: &str| {
        fs::symlink_metadata(dest_replay.join(name))
            .expect("stat replayed file")
            .ino()
    };
    assert_eq!(
        ino("b-leader"),
        ino("m-middle"),
        "cluster members must share one inode after replay"
    );
    assert_eq!(
        ino("b-leader"),
        ino("z-trailer"),
        "cluster members must share one inode after replay"
    );
    assert_ne!(
        ino("b-leader"),
        ino("a-twin"),
        "a same-content file that was never hard-linked must keep its own inode"
    );
}
