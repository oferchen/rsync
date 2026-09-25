//! Cross-implementation oracle for `--write-batch` iflags fidelity on a
//! metadata-only change to an EXISTING non-regular entry: an upstream rsync
//! `--read-batch -i` replay of an oc-produced batch must print the `.d`/`.L`
//! itemize row for a dir/symlink whose attributes changed but whose node was
//! kept.
//!
//! `#[ignore]`d because it requires a locally built upstream rsync 3.5.0
//! (`target/interop/upstream-src/rsync-3.5.0/rsync`), which is not present on
//! a bare checkout; build it with `bash tools/ci/run_interop.sh` or by hand
//! (`git clone --branch v3.5.0 https://github.com/RsyncProject/rsync.git`
//! then `./configure --disable-md2man --disable-openssl --disable-xxhash
//! --disable-zstd --disable-lz4 && make`). Run explicitly with `--ignored`
//! once built. Mirrors the `require_upstream` convention in the sibling
//! `write_batch_upstream_read_batch_itemize.rs`: a missing oracle binary
//! panics loudly rather than silently reporting a pass.
//!
//! Scope note: a special (FIFO/socket/device) reaches the write-side branch
//! through the same non-regular arm as the symlink, and a local copy of a
//! *pre-existing* special node blocks on opening the node (a copy-path
//! limitation unrelated to the batch-record change under test), so it is not
//! exercised here; the dir + symlink cover both arms of the write-side gate.
//!
//! The always-run companion `write_batch_metaonly_nonfile_iflags.rs` pins the
//! same fix at the byte level without needing upstream; this test additionally
//! confirms the itemize rows upstream actually prints.
//!
//! Drives the real `oc-rsync` binary end to end (not `engine::local_copy`
//! directly): the batch trailer (goodbye bytes + stats block) is written by
//! the `core` orchestration layer, so a batch built by calling
//! `engine::local_copy` in isolation would leave an upstream `--read-batch`
//! reader blocked waiting for bytes that never arrive.
//!
//! # Upstream Reference
//!
//! - `generator.c:517-586 itemize()` (report bits), `:584` (`ITEM_IS_NEW`
//!   only for an absent dest), `sender.c:469 write_ndx_and_attrs()`,
//!   `log.c:730-746` (itemize string rendering; `.` fill when a report bit is
//!   set but the entry is neither new nor transferred).

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use filetime::{FileTime, set_file_mtime, set_symlink_file_times};
use tempfile::tempdir;
use test_support::{Deadlined, run_deadlined};

/// Wall-clock budget for the upstream `--read-batch` subprocess.
const UPSTREAM_TIMEOUT: Duration = Duration::from_secs(30);

/// Locally built upstream rsync 3.5.0, source-tree layout.
fn upstream_3_5_0() -> PathBuf {
    PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../target/interop/upstream-src/rsync-3.5.0/rsync"
    ))
}

/// Panics unless `path` is a genuine upstream rsync 3.5.0 binary.
fn require_upstream(path: &Path) {
    if !path.exists() {
        panic!(
            "upstream rsync 3.5.0 required for this oracle test, but {} is absent. \
             Build it with `bash tools/ci/run_interop.sh` or by hand (see this file's \
             module doc). This test is #[ignore]d for that reason - run with --ignored \
             once the binary exists.",
            path.display()
        );
    }
    let Ok(output) = Command::new(path).arg("--version").output() else {
        panic!(
            "{} exists but `--version` failed to execute",
            path.display()
        );
    };
    let banner = String::from_utf8_lossy(&output.stdout);
    let first = banner.lines().next().unwrap_or_default();
    if !(first.starts_with("rsync  version 3.5.0") && first.contains("protocol version")) {
        panic!("{} is not upstream rsync 3.5.0: {first:?}", path.display());
    }
}

/// Populate `root/{adir,alink,tgt}` with the given directory mode and
/// timestamps. `tgt` is always the same bytes at `file_time`, so a matching
/// source and destination `tgt` are quick-check-skipped.
fn build_tree(root: &Path, dir_mode: u32, file_time: FileTime, link_time: FileTime) {
    fs::create_dir_all(root.join("adir")).expect("create dir");
    fs::set_permissions(root.join("adir"), fs::Permissions::from_mode(dir_mode))
        .expect("chmod dir");
    fs::write(root.join("tgt"), b"target\n").expect("write regular file");
    symlink("tgt", root.join("alink")).expect("create symlink");
    set_file_mtime(root.join("adir"), file_time).expect("mtime dir");
    set_file_mtime(root.join("tgt"), file_time).expect("mtime file");
    set_symlink_file_times(root.join("alink"), link_time, link_time).expect("mtime symlink");
}

/// An upstream `--read-batch -i` of an oc batch built over a tree whose dir and
/// symlink pre-exist in the destination with stale metadata must print a
/// `.d`/`.L` itemize row for each - proving the write side carries the
/// metadata-only iflags word upstream expects.
#[test]
#[ignore = "requires a locally built upstream rsync 3.5.0; run with --ignored"]
fn upstream_read_batch_itemizes_kept_nonfile_metadata_changes() {
    let upstream = upstream_3_5_0();
    require_upstream(&upstream);
    let oc_rsync = test_support::oc_rsync_bin();

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("src");
    let dest_write = temp.path().join("dst_write");
    let dest_read = temp.path().join("dst_read");
    let batch_path = temp.path().join("oc.batch");

    let src_time = FileTime::from_unix_time(1_735_689_600, 0); // 2025-01-01
    let dst_time = FileTime::from_unix_time(1_704_067_200, 0); // 2024-01-01
    let src_link_time = FileTime::from_unix_time(1_748_736_000, 0); // 2025-06-01
    let dst_link_time = FileTime::from_unix_time(1_717_200_000, 0); // 2024-06-01

    build_tree(&source, 0o755, src_time, src_link_time);
    // Both destinations start with the same stale entries: the dir + symlink
    // differ (dir in mode + mtime, symlink in mtime), the regular `tgt` is
    // identical, so it is quick-check-skipped.
    build_tree(&dest_write, 0o700, dst_time, dst_link_time);
    build_tree(&dest_read, 0o700, dst_time, dst_link_time);
    set_file_mtime(dest_write.join("tgt"), src_time).expect("align write-side tgt mtime");
    set_file_mtime(dest_read.join("tgt"), src_time).expect("align read-side tgt mtime");

    let mut src_arg = source.clone().into_os_string();
    src_arg.push("/");
    let mut write_cmd = Command::new(&oc_rsync);
    write_cmd
        .arg("-a")
        .arg(format!("--write-batch={}", batch_path.display()))
        .arg(&src_arg)
        .arg(&dest_write);
    let write_outcome =
        run_deadlined(&mut write_cmd, UPSTREAM_TIMEOUT).expect("spawn oc-rsync --write-batch");
    let Deadlined::Finished { status, stderr, .. } = write_outcome else {
        panic!("oc-rsync --write-batch timed out after {UPSTREAM_TIMEOUT:?}");
    };
    assert!(
        status.success(),
        "oc-rsync --write-batch failed: {}",
        String::from_utf8_lossy(&stderr)
    );

    let mut read_cmd = Command::new(&upstream);
    read_cmd
        .arg("-avi")
        .arg(format!("--read-batch={}", batch_path.display()))
        .arg(format!("{}/", dest_read.display()));
    let read_outcome =
        run_deadlined(&mut read_cmd, UPSTREAM_TIMEOUT).expect("spawn upstream --read-batch");
    let Deadlined::Finished {
        status,
        stdout,
        stderr,
    } = read_outcome
    else {
        panic!("upstream --read-batch timed out after {UPSTREAM_TIMEOUT:?}");
    };
    assert!(
        status.success(),
        "upstream --read-batch failed: {}",
        String::from_utf8_lossy(&stderr)
    );
    let stdout = String::from_utf8_lossy(&stdout);

    // rsync `-i` renders a fixed 11-column itemize field `YXcstpoguax`
    // (log.c:730-746) followed by a space and the name. Parse the field by
    // position rather than matching an exact dot/space fill, which varies with
    // which attributes are preserved. For a kept (non-new, non-transferred)
    // entry Y is '.'; a changed attribute shows its letter, an unchanged one a
    // '.'. Columns (0-indexed): 1=type(X), 4=time(t), 5=perms(p).
    let itemize_field = |name_suffix: &str| -> String {
        stdout
            .lines()
            // Itemize output is ASCII, so a byte split at column 11 is a valid
            // char boundary; guard on length so non-itemize lines are skipped.
            .filter(|l| l.is_ascii() && l.len() > 12)
            .find_map(|l| {
                let (field, rest) = l.split_at(11);
                let name = rest.trim_start();
                name.ends_with(name_suffix).then(|| field.to_string())
            })
            .unwrap_or_else(|| {
                panic!(
                    "upstream --read-batch -i printed no 11-column itemize row for `{name_suffix}`; \
                     before the write-side fix oc emitted no delta word for a kept metadata-only \
                     non-file entry, so upstream had no row to print. Got:\n{stdout}"
                )
            })
    };
    // The dir changed mode + mtime -> type 'd', time 't', perms 'p'.
    let dir = itemize_field("adir/");
    assert!(
        &dir[1..2] == "d" && &dir[4..5] == "t" && &dir[5..6] == "p",
        "kept dir must itemize as a time+perms change (X=d,t,p); got itemize field {dir:?}"
    );
    // The symlink changed only its mtime -> type 'L', time 't', perms '.'.
    let link = itemize_field("alink -> tgt");
    assert!(
        &link[1..2] == "L" && &link[4..5] == "t" && &link[5..6] == ".",
        "kept symlink must itemize as a time-only change (X=L,t, no p); got itemize field {link:?}"
    );
    // The byte-identical regular file must be quick-check-skipped: no
    // transferred (`>f`) row for it.
    assert!(
        !stdout
            .lines()
            .any(|l| l.starts_with(">f") && l.trim_end().ends_with(" tgt")),
        "the byte-identical regular file `tgt` must be quick-check-skipped, not \
         transferred; got:\n{stdout}"
    );
}
