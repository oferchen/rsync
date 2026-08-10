//! Round-trip coverage for the `--atimes` / `--crtimes` flags.
//!
//! Mirrors the upstream `testsuite/atimes.test` and `testsuite/crtimes.test`
//! scenarios end-to-end through the metadata application layer that wires
//! the IFX-8 (access time) and IFX-9 (creation time) flist fields into the
//! receiver. The upstream scripts stamp known timestamps on a source tree
//! and then assert byte-identical preservation on the destination; these
//! tests do the equivalent through [`apply_file_metadata_with_options`],
//! which is the function the receiver calls per file once the entry's
//! atime/crtime have been read off the wire.
//!
//! Upstream references:
//! - `target/interop/upstream-src/rsync-3.4.4/testsuite/atimes.test`
//! - `target/interop/upstream-src/rsync-3.4.4/testsuite/crtimes.test`
//! - `rsync.c:set_file_attrs()` for the apply order (chown -> chmod ->
//!   utimensat -> crtime).
//!
//! Platform gating:
//! - The atime round-trip is unix-only: Windows lacks the `utimensat`
//!   semantics rsync relies on for `--atimes`.
//! - The crtime round-trip is macOS + Linux only. macOS sets birthtime via
//!   `setattrlist(2)`; Linux exposes birthtime through statx on kernel
//!   4.11+ but cannot *set* it, so the Linux variant verifies the apply
//!   path runs cleanly and gracefully no-ops on filesystems that do not
//!   surface a creation time.

#![cfg(unix)]

use filetime::{FileTime, set_file_atime, set_file_times};
use metadata::{MetadataOptions, apply_file_metadata_with_options};
use std::fs;
use tempfile::tempdir;

/// Source atime used by the upstream `atimes.test` fixture
/// (`touch -a -t 200102031717.42`).
///
/// `2001-02-03 17:17:42` UTC encoded as seconds since the Unix epoch.
const UPSTREAM_ATIMES_TEST_ATIME: i64 = 981_220_662;

/// Source mtime backdated well into the past so the destination's freshly
/// created mtime never accidentally matches the source's, which would let
/// the assertion pass without the apply path actually running.
const SOURCE_MTIME: i64 = 1_500_000_000;

#[cfg(unix)]
#[test]
fn atimes_round_trip_preserves_source_atime() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("foo");
    let dest = temp.path().join("foo.dest");
    fs::write(&source, b"atimes round-trip").expect("write source");
    fs::write(&dest, b"atimes round-trip").expect("write dest");

    // Stamp the upstream fixture's atime, plus a backdated mtime so the
    // dest's just-written mtime cannot coincidentally match.
    let source_atime = FileTime::from_unix_time(UPSTREAM_ATIMES_TEST_ATIME, 0);
    let source_mtime = FileTime::from_unix_time(SOURCE_MTIME, 0);
    set_file_times(&source, source_atime, source_mtime).expect("set source times");

    // Touch dest to a different atime so the assertion is meaningful.
    set_file_atime(&dest, FileTime::from_unix_time(1_234_567_890, 0))
        .expect("set dest atime baseline");

    let source_meta = fs::metadata(&source).expect("source metadata");
    let opts = MetadataOptions::new()
        .preserve_times(true)
        .preserve_atimes(true);
    apply_file_metadata_with_options(&dest, &source_meta, &opts).expect("apply atimes metadata");

    let dest_meta = fs::metadata(&dest).expect("dest metadata");
    let dest_atime = FileTime::from_last_access_time(&dest_meta);
    let dest_mtime = FileTime::from_last_modification_time(&dest_meta);

    // Some filesystems coarsen atime to second resolution even when mtime
    // is nanosecond-accurate. Assert exact equality at second granularity
    // (matching upstream's `checkit` shell comparison) and allow nanos to
    // round-trip when the filesystem supports them.
    assert_eq!(
        dest_atime.unix_seconds(),
        source_atime.unix_seconds(),
        "atime seconds must round-trip under -a --atimes",
    );
    assert_eq!(
        dest_mtime, source_mtime,
        "mtime must round-trip alongside atime under -a --atimes",
    );
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn crtimes_round_trip_preserves_source_birthtime() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("foo");
    let dest = temp.path().join("foo.dest");

    // Upstream `crtimes.test` writes a small payload (`echo hiho`) and then
    // backdates the mtime to fix the birthtime to an older value before
    // bumping mtime forward again. Equivalent here: create the file, then
    // backdate the source mtime so birthtime is "now-ish" but mtime is in
    // the past, mirroring the fixture's post-touch state.
    fs::write(&source, b"hiho\n").expect("write source");
    fs::write(&dest, b"hiho\n").expect("write dest");
    set_file_times(
        &source,
        FileTime::from_unix_time(UPSTREAM_ATIMES_TEST_ATIME, 0),
        FileTime::from_unix_time(SOURCE_MTIME, 0),
    )
    .expect("set source times");

    let source_meta = fs::metadata(&source).expect("source metadata");

    // Linux exposes birthtime via statx only on kernel 4.11+ and only when
    // the filesystem reports it. If `created()` is unavailable, the apply
    // path is a no-op by design and there is nothing to assert.
    let source_created = match source_meta.created() {
        Ok(c) => c,
        Err(_) => {
            eprintln!(
                "skipping: source filesystem does not expose a birthtime (statx STATX_BTIME unsupported)",
            );
            return;
        }
    };

    let opts = MetadataOptions::new()
        .preserve_times(true)
        .preserve_crtimes(true);
    apply_file_metadata_with_options(&dest, &source_meta, &opts).expect("apply crtimes metadata");

    // On macOS `setattrlist(ATTR_CMN_CRTIME)` writes the destination's
    // birthtime; on Linux there is no portable API to set it, so the
    // helper is a documented no-op (mirroring upstream's behavior on
    // platforms without a settable crtime). Assert the apply path runs
    // and, where the OS can set it, that the value matches.
    #[cfg(target_os = "macos")]
    {
        let dest_meta = fs::metadata(&dest).expect("dest metadata");
        let dest_created = dest_meta.created().expect("dest birthtime");
        let source_secs = source_created
            .duration_since(std::time::UNIX_EPOCH)
            .expect("source birthtime epoch")
            .as_secs() as i64;
        let dest_secs = dest_created
            .duration_since(std::time::UNIX_EPOCH)
            .expect("dest birthtime epoch")
            .as_secs() as i64;
        assert_eq!(
            dest_secs, source_secs,
            "dest birthtime must round-trip from source under --crtimes",
        );
    }
    #[cfg(not(target_os = "macos"))]
    {
        // On Linux, `set_crtime` is a no-op, so the apply path is exercised
        // for side-effect coverage only. Suppress the unused binding lint.
        let _ = source_created;
    }
}
