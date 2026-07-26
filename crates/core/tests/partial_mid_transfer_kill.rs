//! Interop test: SIGTERM mid daemon transfer honours the `--partial` contract.
//!
//! A signal that arrives while the client is blocked reading delta data off
//! the wire must stop the transfer promptly, exit with `RERR_SIGNAL` (20), and
//! leave exactly what upstream leaves:
//!
//! | flags | destination after SIGTERM |
//! |---|---|
//! | `--partial` | the partial bytes at the final path |
//! | none | nothing (see `no_partial_temp_cleanup.rs`) |
//! | `--partial-dir=DIR` | the partial bytes in `DIR`, nothing at the final path |
//!
//! The transfer is made structurally unable to complete by
//! [`common::StalledTransfer`]; SIGINT delivery against the same fixture lives
//! in `sigint_temp_cleanup.rs`.
//!
//! Upstream reference:
//! - `rsync.c:684 sig_int()` - records the signal, `exit_cleanup(RERR_SIGNAL)`
//! - `cleanup.c:159-183` - `cleanup_got_literal && keep_partial` retention,
//!   `handle_partial_dir(PDIR_CREATE)` for `--partial-dir`, modtime 0 stamp
//!   for plain `--partial`
//! - `errcode.h` - `RERR_SIGNAL` is 20

#[cfg(unix)]
mod common;

#[cfg(unix)]
use common::{FORWARD_CAP, StalledTransfer, TEST_FILE_SIZE, upstream_rsync};

#[cfg(unix)]
use std::fs;

/// `--partial` must leave the received prefix at the final destination path.
///
/// upstream: `cleanup.c:167-182` renames the temp onto `cleanup_new_fname`
/// when `keep_partial` is set and literal data arrived.
#[cfg(unix)]
#[test]
fn partial_flag_retains_file_on_mid_transfer_kill() {
    let fixture = StalledTransfer::start();
    let source = fixture.add_file("large.bin", TEST_FILE_SIZE);
    let url = format!("{}/large.bin", fixture.module_url());
    fixture.interrupt(
        &test_support::oc_rsync_bin(),
        &["--partial", &url],
        libc::SIGTERM,
    );

    let dest_file = fixture.dest_path().join("large.bin");
    assert!(
        dest_file.exists(),
        "--partial must leave the partial at the destination; daemon log: {}",
        fixture.daemon_log()
    );

    let partial = fs::read(&dest_file).expect("read partial file");
    assert!(!partial.is_empty(), "retained partial must not be empty");
    assert!(
        partial.len() < TEST_FILE_SIZE,
        "the proxy caps the transfer at {FORWARD_CAP} bytes, so a complete \
         {TEST_FILE_SIZE}-byte file means the interrupt was not what stopped it"
    );
    assert_eq!(
        &partial[..],
        &source[..partial.len()],
        "the retained partial must be a byte-exact prefix of the source"
    );
}

/// Without `--partial` nothing may survive - neither the destination file nor
/// the `.name.XXXXXX` temp the receiver was writing.
///
/// upstream: `cleanup.c:194-197` unlinks `cleanup_fname` when `keep_partial`
/// is unset.
#[cfg(unix)]
#[test]
fn no_partial_flag_cleans_up_on_mid_transfer_kill() {
    let fixture = StalledTransfer::start();
    fixture.add_file("large.bin", TEST_FILE_SIZE);
    let url = format!("{}/large.bin", fixture.module_url());
    fixture.interrupt(&test_support::oc_rsync_bin(), &[&url], libc::SIGTERM);

    let leftovers: Vec<_> = fs::read_dir(fixture.dest_path())
        .expect("read dest dir")
        .filter_map(|e| e.ok())
        .map(|e| e.file_name())
        .collect();
    assert!(
        leftovers.is_empty(),
        "without --partial the destination must be empty after an interrupt; found: {leftovers:?}"
    );
}

/// `--partial-dir=DIR` must put the prefix in `DIR` and leave the final path
/// untouched.
///
/// upstream: `cleanup.c:167` routes the temp through
/// `handle_partial_dir(PDIR_CREATE)`, and unlike plain `--partial` it does not
/// stamp the modtime.
#[cfg(unix)]
#[test]
fn partial_dir_flag_retains_file_in_directory_on_kill() {
    let partial_dir_name = ".rsync-partial";
    let fixture = StalledTransfer::start();
    let source = fixture.add_file("large.bin", TEST_FILE_SIZE);
    let url = format!("{}/large.bin", fixture.module_url());
    fixture.interrupt(
        &test_support::oc_rsync_bin(),
        &[&format!("--partial-dir={partial_dir_name}"), &url],
        libc::SIGTERM,
    );

    let dest_file = fixture.dest_path().join("large.bin");
    let partial_file = fixture.dest_path().join(partial_dir_name).join("large.bin");

    assert!(
        !dest_file.exists(),
        "--partial-dir must not leave anything at the final destination"
    );
    assert!(
        partial_file.exists(),
        "--partial-dir={partial_dir_name} must hold the partial; daemon log: {}",
        fixture.daemon_log()
    );

    let partial = fs::read(&partial_file).expect("read partial file");
    assert!(!partial.is_empty(), "retained partial must not be empty");
    assert!(
        partial.len() < TEST_FILE_SIZE,
        "partial ({} bytes) must be shorter than the {TEST_FILE_SIZE}-byte source",
        partial.len()
    );
    assert_eq!(
        &partial[..],
        &source[..partial.len()],
        "the retained partial must be a byte-exact prefix of the source"
    );
}

/// The same assertions against an upstream client, pinning oc's daemon side
/// and giving the oc-client expectations a live reference.
///
/// Opt-in like every other upstream-requiring test here, because the standard
/// test cells have no upstream rsync (macOS ships openrsync at
/// `/usr/bin/rsync`). Once selected it never skips itself: [`upstream_rsync`]
/// panics naming every path it tried rather than returning green.
#[cfg(unix)]
#[test]
#[ignore = "requires an upstream rsync binary"]
fn upstream_client_partial_retains_file_on_kill() {
    let upstream = upstream_rsync();
    let fixture = StalledTransfer::start();
    let source = fixture.add_file("large.bin", TEST_FILE_SIZE);
    let url = format!("{}/large.bin", fixture.module_url());
    fixture.interrupt(&upstream, &["--partial", &url], libc::SIGTERM);

    let dest_file = fixture.dest_path().join("large.bin");
    assert!(
        dest_file.exists(),
        "upstream --partial must leave the partial at the destination; daemon log: {}",
        fixture.daemon_log()
    );

    let partial = fs::read(&dest_file).expect("read partial file");
    assert!(!partial.is_empty(), "retained partial must not be empty");
    assert!(
        partial.len() < TEST_FILE_SIZE,
        "partial ({} bytes) must be shorter than the {TEST_FILE_SIZE}-byte source",
        partial.len()
    );
    assert_eq!(
        &partial[..],
        &source[..partial.len()],
        "the retained partial must be a byte-exact prefix of the source"
    );
}
