//! Regression test: `--write-batch` must record an `NDX` + iflags entry for
//! every created directory, symlink, and special file (FIFO/socket/device),
//! not only for regular files.
//!
//! `#7929` fixed the regular-file half of the same defect (a fresh file was
//! itemized `>f.........` and counted zero created files by an upstream
//! `--read-batch` peer). A created directory, symlink, or special has no
//! `ITEM_TRANSFER` data, so upstream writes only its `NDX` and the 16-bit
//! iflags word into the batch stream; before this fix oc emitted no entry for
//! them at all. The consequence on an upstream `--read-batch` replay is:
//!
//! - directories/symlinks are created from the flist regardless, but are
//!   dropped from the `--stats` created breakdown and lose their `cd`/`cL`
//!   itemize rows;
//! - a special is worse - the generator expects the entry and, finding none,
//!   aborts the replay with exit 23 (`receiver.c:575 no_batched_update()`)
//!   and never creates the node.
//!
//! This pins the fix at the byte level without an external upstream binary:
//! it drives a real `--write-batch` capture of a tree of purely non-regular
//! entries (so the delta stream is a clean run of `NDX` + 2-byte iflags words
//! with no interleaved sum_head/token bodies) and decodes each word. The
//! `#[ignore]`d companion `write_batch_upstream_read_batch_nonfile.rs`
//! additionally confirms the created counts an upstream reader prints.
//!
//! # Upstream Reference
//!
//! - `generator.c:1480-1482` (directory), `:1605-1610` (symlink),
//!   `:1679-1682` (device/special) - `itemize()` runs with a base of
//!   `ITEM_LOCAL_CHANGE` (dirs) or `ITEM_LOCAL_CHANGE|ITEM_REPORT_CHANGE`
//!   (symlinks/specials) and ORs in `ITEM_IS_NEW` for an absent destination
//!   (`generator.c:583-584`); the word is written with no sum_head because
//!   `ITEM_TRANSFER` is clear.
//! - `receiver.c:742-802` reads the word in the `!(iflags & ITEM_TRANSFER)`
//!   branch and bumps `stats.created_{dirs,symlinks,devices,specials}` under
//!   the `ITEM_IS_NEW` guard.

#![cfg(unix)]

use std::fs;
use std::io::Read;
use std::os::unix::fs::symlink;
use std::process::Command;
use std::sync::{Arc, Mutex};

use batch::{BatchConfig, BatchFlags, BatchMode, BatchReader, BatchWriter};
use engine::local_copy::{LocalCopyExecution, LocalCopyOptions, LocalCopyPlan};
use protocol::CompatibilityFlags;
use protocol::codec::{NdxCodec, NdxCodecEnum};
use tempfile::tempdir;

/// `rsync.h:257` - `#define ITEM_TRANSFER (1<<15)`.
const ITEM_TRANSFER: u16 = 0x8000;
/// A created directory: `ITEM_LOCAL_CHANGE | ITEM_IS_NEW`
/// (`generator.c:1480-1482` + `:583-584`).
const CREATED_DIR: u16 = 0x4000 | 0x2000;
/// A created symlink or special:
/// `ITEM_LOCAL_CHANGE | ITEM_REPORT_CHANGE | ITEM_IS_NEW`
/// (`generator.c:1605-1610` / `:1679-1682` + `:583-584`).
const CREATED_NONREG: u16 = 0x4000 | 0x0002 | 0x2000;

/// Builds a `--write-batch` writer with the header written eagerly, matching
/// the always-run companion `write_batch_new_file_iflags.rs`.
/// `preserve_uid`/`preserve_gid`/`preserve_acls` stay at their `Default`
/// (false) so `write_batch_id_lists()` emits no bytes between the flist end
/// marker and the first entry's `NDX` + iflags word, which is what this test
/// decodes.
fn make_writer(path: &std::path::Path) -> Arc<Mutex<BatchWriter>> {
    let compat_flags = CompatibilityFlags::SAFE_FILE_LIST
        | CompatibilityFlags::AVOID_XATTR_OPTIMIZATION
        | CompatibilityFlags::CHECKSUM_SEED_FIX
        | CompatibilityFlags::INPLACE_PARTIAL_DIR
        | CompatibilityFlags::VARINT_FLIST_FLAGS;
    let config = BatchConfig::new(BatchMode::Write, path.to_string_lossy().into_owned(), 32)
        .with_compat_flags(compat_flags.bits() as i32)
        .with_checksum_seed(1);
    let mut writer = BatchWriter::new(config).expect("create batch writer");
    let flags = BatchFlags {
        recurse: true,
        ..Default::default()
    };
    writer.write_header(flags).expect("write batch header");
    Arc::new(Mutex::new(writer))
}

/// A tree of only non-regular entries (a directory, a nested directory, a
/// symlink, and a FIFO) must record one `NDX` + iflags word per created
/// entry - never a bare flist entry with no delta-stream row.
#[test]
fn write_batch_records_iflags_for_created_dirs_symlinks_and_specials() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("src");
    let dest = temp.path().join("dst");
    let batch_path = temp.path().join("batch.bin");

    fs::create_dir_all(source.join("d1/d2")).expect("create nested source dirs");
    symlink("d1", source.join("link1")).expect("create source symlink");
    // A FIFO exercises the special-file path; it needs no root, unlike a
    // device node, and travels the same itemize/created-count route.
    let fifo = source.join("afifo");
    let status = Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .expect("spawn mkfifo");
    assert!(status.success(), "mkfifo failed");

    fs::create_dir_all(&dest).expect("create dest dir");

    let writer = make_writer(&batch_path);
    let options = LocalCopyOptions::default()
        .recursive(true)
        .links(true)
        .specials(true)
        .batch_writer(Some(Arc::clone(&writer)));

    let mut src_os = source.clone().into_os_string();
    src_os.push("/");
    let operands = vec![src_os, dest.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("write-batch transfer succeeds");

    Arc::try_unwrap(writer)
        .expect("writer uniquely owned")
        .into_inner()
        .expect("writer mutex not poisoned")
        .finalize()
        .expect("finalize batch writer");

    // The destination really is populated (this is not a dry run).
    assert!(dest.join("d1/d2").is_dir(), "nested dir must be created");
    assert!(
        fs::symlink_metadata(dest.join("link1"))
            .expect("stat replayed symlink")
            .file_type()
            .is_symlink(),
        "symlink must be created"
    );

    let read_cfg = BatchConfig::new(
        BatchMode::Read,
        batch_path.to_string_lossy().into_owned(),
        32,
    );
    let mut reader = BatchReader::new(read_cfg).expect("open batch reader");
    reader.read_header().expect("read batch header");
    let entries = reader
        .read_protocol_flist()
        .expect("decode flist after --write-batch");
    // The flist carries every entry (root ".", the two dirs, the symlink, the
    // FIFO); the delta stream is what this test scrutinises.
    assert!(
        entries.iter().any(|e| e.name() == "afifo"),
        "flist must contain the FIFO entry"
    );

    // No id-list bytes are emitted (no preserve_uid/gid/acls), so the bytes
    // after the flist are a run of `NDX` + 2-byte iflags words - no sum_head
    // or token body, because none of these entries carry ITEM_TRANSFER -
    // terminated by NDX_DONE (rsync.h:285, -1).
    let mut body = reader.into_body().expect("batch body reader");
    let mut ndx_codec = NdxCodecEnum::new(32);
    let mut iflags_seen = Vec::new();
    loop {
        match NdxCodec::read_ndx(&mut ndx_codec, &mut body) {
            Ok(ndx) if ndx >= 0 => {
                let mut word = [0u8; 2];
                body.read_exact(&mut word).expect("read iflags word");
                let iflags = u16::from_le_bytes(word);
                assert_eq!(
                    iflags & ITEM_TRANSFER,
                    0,
                    "a created dir/symlink/special carries no ITEM_TRANSFER data, so no \
                     sum_head follows its iflags word; got {iflags:#06x} at ndx {ndx}"
                );
                iflags_seen.push(iflags);
            }
            // NDX_DONE (-1) or end of the entry run.
            _ => break,
        }
    }

    iflags_seen.sort_unstable();
    assert_eq!(
        iflags_seen,
        vec![CREATED_DIR, CREATED_DIR, CREATED_NONREG, CREATED_NONREG],
        "the delta stream must carry one iflags word per created non-regular entry: \
         two directories (d1, d1/d2) at {CREATED_DIR:#06x} \
         (ITEM_LOCAL_CHANGE|ITEM_IS_NEW) and the symlink + FIFO at {CREATED_NONREG:#06x} \
         (ITEM_LOCAL_CHANGE|ITEM_REPORT_CHANGE|ITEM_IS_NEW). A missing entry makes an \
         upstream --read-batch peer under-count dirs/symlinks and abort on the special \
         (receiver.c:575 no_batched_update). The transfer root \".\" is not itemized as \
         created and must not appear. Got {iflags_seen:?}"
    );
}
