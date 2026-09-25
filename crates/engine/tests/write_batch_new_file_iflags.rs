//! Regression test: `--write-batch` must record `ITEM_IS_NEW` for a file
//! created at a destination that did not previously exist.
//!
//! Prior to the fix, `begin_batch_file_delta()` unconditionally wrote the
//! bare `ITEM_TRANSFER` bit (0x8000) as the per-file iflags word and never
//! patched in `ITEM_IS_NEW` (0x2000), regardless of whether the destination
//! existed before the transfer. Because upstream's sender re-emits this
//! exact iflags word to whichever peer is reading the batch
//! (`sender.c:469 write_ndx_and_attrs()`), and only increments
//! `stats.created_files` when `ITEM_IS_NEW` is set (`sender.c:587,625`), an
//! upstream `--read-batch` replay of an oc-produced batch itemized a brand
//! new file as unchanged (`>f.........`) and counted zero created files
//! instead of `>f+++++++++` / one created file.
//!
//! This test pins the byte-level fix directly: it drives a real
//! `--write-batch` capture through the engine's local-copy executor for a
//! single new file and decodes the raw NDX + iflags word from the batch
//! body, without depending on an external upstream binary. The cross-impl
//! oracle test (`write_batch_upstream_read_batch_itemize.rs`, `#[ignore]`d,
//! requires a locally built upstream rsync 3.5.0) additionally pins the
//! itemize string and `--stats` count upstream actually produces.
//!
//! # Upstream Reference
//!
//! - `generator.c:583-584 itemize()` - `iflags |= ITEM_IS_NEW` when
//!   `statret < 0` (destination absent).
//! - `sender.c:469 write_ndx_and_attrs()` - re-emits the iflags word to the
//!   peer, which is what a batch file tees.
//! - `sender.c:587,625` - `stats.created_files++` gated on
//!   `iflags & ITEM_IS_NEW`.

use std::fs;
use std::io::Read;
use std::sync::{Arc, Mutex};

use batch::{BatchConfig, BatchFlags, BatchMode, BatchReader, BatchWriter};
use engine::local_copy::{LocalCopyExecution, LocalCopyOptions, LocalCopyPlan};
use protocol::CompatibilityFlags;
use protocol::codec::{NdxCodec, NdxCodecEnum};
use tempfile::tempdir;

/// `rsync.h:257` - `#define ITEM_TRANSFER (1<<15)`.
const ITEM_TRANSFER: u16 = 0x8000;
/// `rsync.h:257` - `#define ITEM_IS_NEW (1<<13)`.
const ITEM_IS_NEW: u16 = 0x2000;

/// Builds a `--write-batch` writer with the header written eagerly, mirroring
/// the compat_flags `cli::frontend::execution::drive::workflow::run::
/// local_batch_compat_flags` assembles for a plain `-a --write-batch`
/// invocation. `preserve_uid`/`preserve_gid`/`preserve_acls` are left at
/// their `Default` (false) so `write_batch_id_lists()` emits no bytes
/// between the flist end marker and the first file's NDX + iflags word,
/// which is what this test decodes.
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

/// A file created at a destination that did not previously exist must carry
/// `ITEM_IS_NEW` in its recorded iflags word.
#[test]
fn write_batch_marks_new_file_item_is_new() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("src");
    let dest = temp.path().join("dst");
    let batch_path = temp.path().join("batch.bin");

    fs::create_dir_all(&source).expect("create source dir");
    // Destination directory exists, but `file.txt` itself does not - this is
    // the "fresh-file transfer" the ledger's reproduction describes.
    fs::create_dir_all(&dest).expect("create dest dir");
    fs::write(source.join("file.txt"), b"hello world\n").expect("write source file");

    let writer = make_writer(&batch_path);
    let options = LocalCopyOptions::default()
        .recursive(true)
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

    assert_eq!(
        fs::read(dest.join("file.txt")).expect("read transferred file"),
        b"hello world\n",
        "the real (non-dry-run) transfer must still write the destination file"
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
    assert!(
        entries.iter().any(|e| e.name() == "file.txt"),
        "flist must contain file.txt; got {:?}",
        entries
            .iter()
            .map(protocol::flist::FileEntry::name)
            .collect::<Vec<_>>()
    );

    // No id-list bytes are emitted (preserve_uid/gid/acls all false), so the
    // very next bytes after the flist are the first (only) file's NDX,
    // decoded with a fresh codec exactly as `flush_batch_delta_to_batch()`
    // encoded it, followed immediately by the 2-byte LE iflags word.
    let mut body = reader.into_body().expect("batch body reader");
    let mut ndx_codec = NdxCodecEnum::new(32);
    let ndx = NdxCodec::read_ndx(&mut ndx_codec, &mut body).expect("read file NDX");
    assert!(ndx >= 0, "expected a real file NDX, got {ndx}");

    let mut iflags_bytes = [0u8; 2];
    body.read_exact(&mut iflags_bytes)
        .expect("read iflags word");
    let iflags = u16::from_le_bytes(iflags_bytes);

    assert_eq!(
        iflags & ITEM_TRANSFER,
        ITEM_TRANSFER,
        "iflags must carry ITEM_TRANSFER for a captured whole-file transfer; got {iflags:#06x}"
    );
    assert_eq!(
        iflags & ITEM_IS_NEW,
        ITEM_IS_NEW,
        "a file created at a destination that did not previously exist must carry \
         ITEM_IS_NEW (0x2000) in the recorded iflags word (upstream generator.c:583-584 \
         itemize() / sender.c:469 write_ndx_and_attrs()); without it a replaying \
         upstream --read-batch itemizes the file as unchanged (`>f.........`) and \
         counts zero created files; got {iflags:#06x}"
    );
}
