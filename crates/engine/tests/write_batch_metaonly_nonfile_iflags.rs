//! Regression test: `--write-batch` must record an `NDX` + iflags entry for a
//! metadata-only change to an EXISTING directory or symlink, not only for a
//! newly created one.
//!
//! The sibling `write_batch_nonfile_iflags.rs` pins the *created* half: a fresh
//! dir/symlink/special gets its `ITEM_LOCAL_CHANGE | ITEM_IS_NEW` word. This
//! pins the *kept* half. When the destination already holds the entry and only
//! its attributes differ (perms, mtime, ...), upstream's `itemize()` still
//! writes an `NDX` + a 16-bit iflags word carrying just the `ITEM_REPORT_*`
//! bits - no `ITEM_IS_NEW`, no `ITEM_LOCAL_CHANGE`, no `ITEM_TRANSFER`, so no
//! sum_head follows. Before this fix oc emitted no delta-stream row at all for
//! such an entry, so an upstream `--read-batch -i` of an oc batch showed no
//! `.d`/`.L` itemize row for a dir/symlink whose metadata it in fact updated.
//! The node and its metadata still apply from the flist, so this is `-i`
//! fidelity only - no data, exit-code, or created-count change.
//!
//! Scope of this always-run test:
//!
//! - A directory exercises the `is_directory()` arm of the write-side gate; a
//!   symlink exercises the `metadata().kind() != File` arm - the two arms that
//!   decide a kept entry is non-regular. A special (FIFO/socket/device) reaches
//!   the write-side branch through the *same* `kind() != File` arm as the
//!   symlink, so it adds no distinct branch to exercise here. It is not driven
//!   by an automated test because a local copy of a *pre-existing* special node
//!   blocks on opening the node (a copy-path limitation unrelated to the
//!   batch-record change under test); its `.S` row was confirmed by hand with
//!   an upstream `--read-batch -i` (`.S..tp afifo`).
//! - A regular file's metadata-only change is a separate, broader gap (its word
//!   rides the delta entry, and `ITEM_REPORT_SIZE`/`TIMEFAIL` bit handling
//!   differs by type); it stays out of scope and is not exercised.
//!
//! This pins the fix at the byte level without an external upstream binary: it
//! drives a real `--write-batch` capture whose dir and symlink pre-exist in the
//! destination with stale metadata (and whose one regular file is byte- and
//! mtime-identical, so quick-check skips it and leaves no word), then decodes
//! each `NDX` + 2-byte iflags word.
//!
//! # Upstream Reference
//!
//! - `generator.c:517-586 itemize()` computes the report bits (`rsync.h:244-254`):
//!   `ITEM_REPORT_TIME` (1<<3) when the mtime differs, `ITEM_REPORT_PERMS`
//!   (1<<4) when the mode differs, and so on; `ITEM_REPORT_SIZE` (1<<2) is set
//!   only for a regular file (`generator.c:527-528` `S_ISREG`) and the same bit
//!   is `ITEM_REPORT_TIMEFAIL` for a symlink, so it must not appear on a
//!   non-regular entry.
//! - `generator.c:584` ORs in `ITEM_IS_NEW` only for an absent destination;
//!   a kept entry therefore carries none of `ITEM_IS_NEW` (1<<13),
//!   `ITEM_LOCAL_CHANGE` (1<<14), or `ITEM_TRANSFER` (1<<15).
//! - `receiver.c:742-802` reads the word in the `!(iflags & ITEM_TRANSFER)`
//!   branch and, with `ITEM_IS_NEW` clear, itemizes the change without bumping
//!   any `created_*` counter.

#![cfg(unix)]

use std::fs;
use std::io::Read;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::sync::{Arc, Mutex};

use batch::{BatchConfig, BatchFlags, BatchMode, BatchReader, BatchWriter};
use engine::local_copy::{LocalCopyExecution, LocalCopyOptions, LocalCopyPlan};
use filetime::{FileTime, set_file_mtime, set_symlink_file_times};
use protocol::CompatibilityFlags;
use protocol::codec::{NdxCodec, NdxCodecEnum};
use tempfile::tempdir;

/// `rsync.h:259` - `#define ITEM_TRANSFER (1<<15)`.
const ITEM_TRANSFER: u16 = 0x8000;
/// `rsync.h:258` - `#define ITEM_LOCAL_CHANGE (1<<14)`.
const ITEM_LOCAL_CHANGE: u16 = 0x4000;
/// `rsync.h:257` - `#define ITEM_IS_NEW (1<<13)`.
const ITEM_IS_NEW: u16 = 0x2000;
/// `rsync.h:248` - `#define ITEM_REPORT_TIME (1<<3)`.
const ITEM_REPORT_TIME: u16 = 1 << 3;
/// `rsync.h:249` - `#define ITEM_REPORT_PERMS (1<<4)`.
const ITEM_REPORT_PERMS: u16 = 1 << 4;
/// A kept symlink whose only change is its mtime: `ITEM_REPORT_TIME`.
const KEPT_TIME: u16 = ITEM_REPORT_TIME;
/// A kept dir whose mode and mtime both differ:
/// `ITEM_REPORT_TIME | ITEM_REPORT_PERMS`.
const KEPT_TIME_PERMS: u16 = ITEM_REPORT_TIME | ITEM_REPORT_PERMS;

/// Builds a `--write-batch` writer with the header written eagerly, matching
/// the created-entry sibling `write_batch_nonfile_iflags.rs`. No
/// uid/gid/acl preservation, so `write_batch_id_lists()` emits no bytes
/// between the flist end marker and the first entry's `NDX` + iflags word.
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

/// A destination that already holds every entry the source does, so each is a
/// *kept* (`MetadataReused`) entry, never a created one:
/// - `adir/` differs from the source in both mode and mtime;
/// - `alink -> tgt` differs only in mtime (same target);
/// - `tgt` is byte- and mtime-identical, so quick-check skips it and it
///   contributes no delta-stream word.
///
/// The word run this decodes is therefore exactly the two non-regular kept
/// entries - a regression that dropped either, or that mislabelled one as
/// created, changes the decoded multiset.
#[test]
fn write_batch_records_metadata_only_iflags_for_kept_dirs_and_symlinks() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("src");
    let dest = temp.path().join("dst");
    let batch_path = temp.path().join("batch.bin");

    // Fixed timestamps: source newer than the stale destination, so mtime
    // comparison deterministically flags a change without depending on
    // wall-clock or filesystem granularity.
    let src_time = FileTime::from_unix_time(1_735_689_600, 0); // 2025-01-01
    let dst_time = FileTime::from_unix_time(1_704_067_200, 0); // 2024-01-01
    let src_link_time = FileTime::from_unix_time(1_748_736_000, 0); // 2025-06-01
    let dst_link_time = FileTime::from_unix_time(1_717_200_000, 0); // 2024-06-01

    // --- source tree ---
    fs::create_dir_all(source.join("adir")).expect("create source dir");
    fs::set_permissions(source.join("adir"), fs::Permissions::from_mode(0o755))
        .expect("chmod source dir");
    fs::write(source.join("tgt"), b"target\n").expect("write source regular file");
    symlink("tgt", source.join("alink")).expect("create source symlink");
    set_file_mtime(source.join("adir"), src_time).expect("mtime source dir");
    set_file_mtime(source.join("tgt"), src_time).expect("mtime source file");
    set_symlink_file_times(source.join("alink"), src_link_time, src_link_time)
        .expect("mtime source symlink");

    // --- stale pre-existing destination (same entries, older metadata) ---
    fs::create_dir_all(dest.join("adir")).expect("create dest dir");
    fs::set_permissions(dest.join("adir"), fs::Permissions::from_mode(0o700))
        .expect("chmod dest dir");
    fs::write(dest.join("tgt"), b"target\n").expect("write dest regular file");
    symlink("tgt", dest.join("alink")).expect("create dest symlink");
    set_file_mtime(dest.join("adir"), dst_time).expect("mtime dest dir");
    // `tgt` must match the source exactly so quick-check skips it (no word).
    set_file_mtime(dest.join("tgt"), src_time).expect("mtime dest file");
    set_symlink_file_times(dest.join("alink"), dst_link_time, dst_link_time)
        .expect("mtime dest symlink");

    let writer = make_writer(&batch_path);
    // perms + times must be preserved so the mode/mtime differences are
    // detected as metadata changes (a `MetadataReused` action), which is what
    // the write-side branch keys on. No owner/group, so no id-list bytes.
    let options = LocalCopyOptions::default()
        .recursive(true)
        .links(true)
        .permissions(true)
        .times(true)
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

    // The destination metadata really was updated (this is not a dry run).
    assert_eq!(
        fs::symlink_metadata(dest.join("adir"))
            .expect("stat dest dir")
            .permissions()
            .mode()
            & 0o777,
        0o755,
        "kept dir must have its mode updated to the source's"
    );

    let read_cfg = BatchConfig::new(
        BatchMode::Read,
        batch_path.to_string_lossy().into_owned(),
        32,
    );
    let mut reader = BatchReader::new(read_cfg).expect("open batch reader");
    reader.read_header().expect("read batch header");
    let _entries = reader
        .read_protocol_flist()
        .expect("decode flist after --write-batch");

    // No id-list bytes are emitted, so the bytes after the flist are a run of
    // `NDX` + 2-byte iflags words - no sum_head or token body, because none of
    // these kept entries carry ITEM_TRANSFER - terminated by NDX_DONE.
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
                    "a kept dir/symlink carries no ITEM_TRANSFER data, so no sum_head \
                     follows its iflags word; got {iflags:#06x} at ndx {ndx}"
                );
                assert_eq!(
                    iflags & (ITEM_IS_NEW | ITEM_LOCAL_CHANGE),
                    0,
                    "a metadata-only kept entry must carry neither ITEM_IS_NEW nor \
                     ITEM_LOCAL_CHANGE (those mark a created/locally-created entry); \
                     got {iflags:#06x} at ndx {ndx}"
                );
                iflags_seen.push(iflags);
            }
            // NDX_DONE (-1) or the end of the entry run.
            _ => break,
        }
    }

    iflags_seen.sort_unstable();
    assert_eq!(
        iflags_seen,
        vec![KEPT_TIME, KEPT_TIME_PERMS],
        "the delta stream must carry one metadata-only iflags word per kept non-regular \
         entry whose attributes changed: the symlink at {KEPT_TIME:#06x} \
         (ITEM_REPORT_TIME) and the dir at {KEPT_TIME_PERMS:#06x} \
         (ITEM_REPORT_TIME|ITEM_REPORT_PERMS). The byte-identical regular file `tgt` is \
         quick-check-skipped and must contribute no word, and ITEM_REPORT_SIZE (1<<2) \
         must never appear (it is regular-file-only / TIMEFAIL for symlinks). A missing \
         word makes an upstream --read-batch -i peer drop the `.d`/`.L` row. \
         Got {iflags_seen:?}"
    );
}
