//! Interop test: SIGTERM mid daemon transfer leaves no residue without `--partial`.
//!
//! Default rsync writes each file to `.name.XXXXXX` and renames it on
//! completion. An interrupt must remove that temp: `cleanup.c:194-197` unlinks
//! `cleanup_fname` whenever `keep_partial` is unset. The failure this guards
//! against is an orphaned temp file - the destination looks clean in a casual
//! `ls` while a hidden partial silently consumes the disk.
//!
//! The transfer is made structurally unable to complete by
//! [`common::StalledTransfer`], so the interrupt is gated on observed on-disk
//! progress rather than on a sleep.
//!
//! Upstream reference:
//! - `cleanup.c:159-197` - retention gate and the unlink that follows it
//! - `receiver.c` - temp file naming pattern `.filename.XXXXXX`
//! - `errcode.h` - `RERR_SIGNAL` is 20

#[cfg(unix)]
mod common;

#[cfg(unix)]
use common::{StalledTransfer, TEST_FILE_SIZE, find_temp_files, upstream_rsync};

#[cfg(unix)]
use std::fs;

/// Single file, no `--partial`: neither the destination nor the temp survives.
#[cfg(unix)]
#[test]
fn no_partial_single_file_no_residue_on_kill() {
    let fixture = StalledTransfer::start();
    fixture.add_file("large.bin", TEST_FILE_SIZE);
    let url = format!("{}/large.bin", fixture.module_url());
    fixture.interrupt(&test_support::oc_rsync_bin(), &[&url], libc::SIGTERM);

    assert!(
        !fixture.dest_path().join("large.bin").exists(),
        "without --partial the destination file must not exist after an interrupt"
    );
    let orphans = find_temp_files(fixture.dest_path());
    assert!(
        orphans.is_empty(),
        "temp file orphans left behind: {orphans:?}"
    );
}

/// Recursive pull of several files: whatever completed may stay, but it must
/// be complete, and no temp may survive anywhere in the tree.
#[cfg(unix)]
#[test]
fn no_partial_multi_file_no_residue_on_kill() {
    let files = [
        ("small.txt", 256_usize),
        ("medium.bin", 64 * 1024),
        ("large_a.bin", TEST_FILE_SIZE),
        ("large_b.bin", TEST_FILE_SIZE),
    ];
    let fixture = StalledTransfer::start();
    for (name, size) in &files {
        fixture.add_file(name, *size);
    }
    let url = format!("{}/", fixture.module_url());
    fixture.interrupt(&test_support::oc_rsync_bin(), &["-r", &url], libc::SIGTERM);

    let orphans = find_temp_files(fixture.dest_path());
    assert!(
        orphans.is_empty(),
        "temp file orphans found after multi-file kill: {orphans:?}"
    );

    for (name, size) in &files {
        let dest_file = fixture.dest_path().join(name);
        if dest_file.exists() {
            let actual = fs::metadata(&dest_file).expect("stat dest file").len() as usize;
            assert_eq!(
                actual, *size,
                "{name} survived the interrupt at {actual} of {size} bytes; without --partial \
                 an incomplete file must be removed, not left looking finished"
            );
        }
    }
}

/// Nested tree: directories are created eagerly and legitimately remain, but
/// no `.name.XXXXXX` may be left at any depth.
#[cfg(unix)]
#[test]
fn no_partial_preserves_dirs_but_removes_temps() {
    let fixture = StalledTransfer::start();
    for name in ["root.bin", "subdir/nested.bin", "subdir/deep/leaf.bin"] {
        fixture.add_file(name, TEST_FILE_SIZE);
    }
    let url = format!("{}/", fixture.module_url());
    fixture.interrupt(&test_support::oc_rsync_bin(), &["-r", &url], libc::SIGTERM);

    let orphans = find_temp_files(fixture.dest_path());
    assert!(
        orphans.is_empty(),
        "temp file orphans found in nested tree: {orphans:?}"
    );

    for name in ["root.bin", "subdir/nested.bin", "subdir/deep/leaf.bin"] {
        let dest_file = fixture.dest_path().join(name);
        if dest_file.exists() {
            let actual = fs::metadata(&dest_file).expect("stat").len() as usize;
            assert_eq!(
                actual, TEST_FILE_SIZE,
                "{name} survived the interrupt at {actual} of {TEST_FILE_SIZE} bytes"
            );
        }
    }
}

/// The same expectation against an upstream client, pinning oc's daemon side.
///
/// Opt-in like every other upstream-requiring test here, because the standard
/// test cells have no upstream rsync (macOS ships openrsync at
/// `/usr/bin/rsync`). Once selected it never skips itself: [`upstream_rsync`]
/// panics naming every path it tried rather than returning green.
#[cfg(unix)]
#[test]
#[ignore = "requires an upstream rsync binary"]
fn upstream_client_no_partial_cleans_up_on_kill() {
    let upstream = upstream_rsync();
    let fixture = StalledTransfer::start();
    fixture.add_file("large.bin", TEST_FILE_SIZE);
    let url = format!("{}/large.bin", fixture.module_url());
    fixture.interrupt(&upstream, &[&url], libc::SIGTERM);

    assert!(
        !fixture.dest_path().join("large.bin").exists(),
        "upstream without --partial must not leave a destination file; daemon log: {}",
        fixture.daemon_log()
    );
    let orphans = find_temp_files(fixture.dest_path());
    assert!(
        orphans.is_empty(),
        "upstream left temp file orphans: {orphans:?}"
    );
}

#[cfg(unix)]
#[cfg(test)]
mod temp_file_pattern_tests {
    use crate::common::is_temp_file_name;

    /// The scanner must recognise the `.name.XXXXXX` shape rsync actually
    /// produces, including names that already contain dots.
    #[test]
    fn detects_rsync_temp_pattern() {
        assert!(is_temp_file_name(".large.bin.a1b2c3"));
        assert!(is_temp_file_name(".photo.jpg.D4E5F6"));
        assert!(is_temp_file_name(".a.b.c.d.AbCdEf"));
    }

    /// Ordinary dotfiles and short/long suffixes must not be reported as
    /// orphans, or the cleanup assertions would fail on innocent files.
    #[test]
    fn rejects_non_temp_names() {
        assert!(!is_temp_file_name(".bashrc"));
        assert!(!is_temp_file_name(".rsync-filter"));
        assert!(!is_temp_file_name("large.bin"));
        assert!(!is_temp_file_name(".large.bin.a1b2c"));
        assert!(!is_temp_file_name(".large.bin.a1b2c3d"));
        assert!(!is_temp_file_name(".large.bin.a1b2c-"));
    }
}
