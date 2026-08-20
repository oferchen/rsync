//! A file appended to between the file-list scan and the data send must be
//! transferred whole, not truncated to its scan-recorded length.
//!
//! Upstream sizes the transfer from the OPENED handle, not from the file list:
//!
//! ```c
//! /* sender.c - for each requested file */
//! if (do_fstat(fd, &st) != 0) { ... }
//! mbuf = map_file(fd, st.st_size, ...);
//! match_sums(f_out, s, mbuf, st.st_size);
//! ```
//!
//! so the length recorded when the tree was scanned is a basis for DECISIONS
//! (skip, delta, sparse) and never a ceiling on the bytes moved. A live log
//! written to during a nightly backup is the everyday case.
//!
//! oc's local-copy executor passed the scan-recorded length straight through as
//! the copy bound, so the copy loop stopped once it had moved that many bytes
//! and the destination was silently short - exit 0, no warning.
//!
//! The window is opened deterministically, not by sleeping: `aaa_big` sorts
//! first, so it is sent first, and `--bwlimit` makes that take seconds. The
//! appender waits for the destination directory to become non-empty, which
//! happens only once the whole file list is built (`--no-inc-recursive`) and
//! the data phase has begun - so the append provably lands after the scan and
//! well before `zzz_grow` is reached. A fixed sleep cannot promise either edge.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use tempfile::TempDir;

/// Scan-recorded length of the growing file.
const ORIGINAL_LEN: usize = 4096;
/// Bytes appended inside the scan-to-send window.
const APPENDED_LEN: usize = 256 * 1024;
/// Throttled leading file; large enough that its transfer spans the append.
const LEADING_LEN: usize = 6 * 1024 * 1024;
/// Longest the appender waits for the data phase to start before giving up.
const WINDOW_TIMEOUT: Duration = Duration::from_secs(60);

fn oc_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_oc-rsync"))
}

fn upstream_binary() -> Option<PathBuf> {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("target/interop/upstream-src/rsync-3.5.0/rsync");
    path.is_file().then_some(path)
}

fn write_filler(path: &Path, len: usize) {
    let mut file = File::create(path).expect("create fixture");
    // Non-repeating bytes so a delta pass cannot match blocks by accident.
    let block: Vec<u8> = (0..len).map(|i| (i.wrapping_mul(31) % 251) as u8).collect();
    file.write_all(&block).expect("write fixture");
    file.sync_all().expect("sync fixture");
}

struct Outcome {
    status: i32,
    stderr: String,
    /// Length of the source once the appender had finished.
    final_source_len: u64,
    destination_len: Option<u64>,
    /// False when the append could not be placed inside the transfer window.
    window_observed: bool,
}

/// Runs one transfer with an append injected into the scan-to-send window.
fn transfer_with_growing_source(binary: &Path) -> Outcome {
    let temp = TempDir::new().expect("tempdir");
    let source = temp.path().join("src");
    let destination = temp.path().join("dst");
    fs::create_dir(&source).expect("create src");
    fs::create_dir(&destination).expect("create dst");

    write_filler(&source.join("aaa_big"), LEADING_LEN);
    let growing = source.join("zzz_grow");
    write_filler(&growing, ORIGINAL_LEN);

    let observed = Arc::new(AtomicBool::new(false));
    let appender = {
        let growing = growing.clone();
        let destination = destination.clone();
        let observed = Arc::clone(&observed);
        thread::spawn(move || {
            // The condition, not a sleep: anything in the destination means the
            // file list is complete and the data phase has started.
            let deadline = Instant::now() + WINDOW_TIMEOUT;
            while Instant::now() < deadline {
                let started = fs::read_dir(&destination)
                    .map(|mut entries| entries.next().is_some())
                    .unwrap_or(false);
                if started {
                    observed.store(true, Ordering::SeqCst);
                    break;
                }
                thread::sleep(Duration::from_millis(5));
            }
            if !observed.load(Ordering::SeqCst) {
                return;
            }
            let mut file = OpenOptions::new()
                .append(true)
                .open(&growing)
                .expect("reopen growing source");
            let block: Vec<u8> = (0..APPENDED_LEN).map(|i| (i % 253) as u8).collect();
            file.write_all(&block).expect("append to growing source");
            file.sync_all().expect("sync growing source");
        })
    };

    let output = Command::new(binary)
        .arg("-a")
        .arg("--no-inc-recursive")
        .arg("--bwlimit=1500")
        .arg(format!("{}/", source.display()))
        .arg(format!("{}/", destination.display()))
        .output()
        .expect("run rsync");
    appender.join().expect("appender thread");

    let copied = destination.join("zzz_grow");
    Outcome {
        status: output.status.code().unwrap_or(-1),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        final_source_len: fs::metadata(&growing).expect("stat source").len(),
        destination_len: fs::metadata(&copied).ok().map(|meta| meta.len()),
        window_observed: observed.load(Ordering::SeqCst),
    }
}

fn assert_transferred_whole(label: &str, outcome: &Outcome) {
    assert!(
        outcome.window_observed,
        "{label}: the transfer never reached its data phase, so the append was \
         never placed inside the scan-to-send window - this run proves nothing"
    );
    let expected = (ORIGINAL_LEN + APPENDED_LEN) as u64;
    assert_eq!(
        outcome.final_source_len, expected,
        "{label}: fixture did not grow as intended"
    );
    assert_eq!(
        outcome.status, 0,
        "{label}: exited {} - {}",
        outcome.status, outcome.stderr
    );
    assert_eq!(
        outcome.destination_len,
        Some(expected),
        "{label}: destination holds {:?} bytes, source ended at {expected}. A \
         value of {ORIGINAL_LEN} means the transfer was bounded by the \
         scan-recorded length instead of the opened file's size.",
        outcome.destination_len
    );
}

/// The defect: the destination must hold the file's FINAL size, not the length
/// the scan recorded. Asserting the full size (rather than merely "more than
/// the recorded length") is what makes a partial fix fail here too.
#[test]
fn growing_source_file_is_transferred_whole() {
    assert_transferred_whole("oc-rsync", &transfer_with_growing_source(&oc_binary()));
}

/// Cross-implementation control. Without it the assertion above is only oc's
/// own opinion of correct; with it, the same fixture is shown to behave the
/// same way on the implementation oc mirrors.
#[test]
fn upstream_transfers_a_growing_source_file_whole() {
    let Some(upstream) = upstream_binary() else {
        println!(
            "skipping: no built rsync 3.5.0 at \
             target/interop/upstream-src/rsync-3.5.0/rsync"
        );
        return;
    };
    assert_transferred_whole("rsync 3.5.0", &transfer_with_growing_source(&upstream));
}
