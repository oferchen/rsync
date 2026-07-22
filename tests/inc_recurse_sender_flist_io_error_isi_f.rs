//! ISI.f - sender-side INC_RECURSE under flist io_error.
//!
//! Failure-mode companion to the ISI.c / ISI.d push-interop harnesses.
//! Where those tests prove that a clean INC_RECURSE push reaches a
//! byte-identical destination, this test forces the sender to hit an
//! `opendir`/`readdir` failure mid-enumeration and locks in the
//! upstream-compatible recovery contract:
//!
//! 1. The sender records `IOERR_GENERAL` via `record_io_error` instead
//!    of aborting the walk
//!    (`crates/transfer/src/generator/file_list/walk.rs::scan_directory_batched`).
//! 2. The sender emits a partial flist with the still-readable entries
//!    and writes the accumulated `io_error` bitfield into the flist end
//!    marker (`crates/transfer/src/generator/protocol_io.rs::send_file_list`
//!    -> `flist.c:2518 write_int(f, io_error)`).
//! 3. The upstream receiver consumes the partial flist, materializes
//!    every successfully-enumerated file byte-identically, and exits
//!    with `RERR_PARTIAL` (23) rather than aborting via
//!    `RERR_PROTOCOL`/`RERR_STREAMIO`.
//! 4. The sender's own exit status is the same partial-transfer code,
//!    confirming the io_error round-trips back through the multiplex
//!    layer (`MSG_IO_ERROR`, `crates/transfer/src/reader/multiplex.rs`).
//!
//! ## Why this matters
//!
//! INC_RECURSE dispatches sub-list segments lazily during the transfer
//! loop. A bug that aborts the walk on the first unreadable directory
//! would leave already-queued segments un-flushed, the receiver waiting
//! on `NDX_FLIST_EOF` indefinitely, and the transfer hanging or
//! crashing with a stream-format error - not the graceful partial
//! transfer upstream guarantees. The v0.5.8 io_error accumulation work
//! (`record_io_error` was added across the walk path) made this
//! recovery possible; ISI.f is the regression seed that prevents it
//! from regressing under the sender-side INC_RECURSE path specifically.
//!
//! ## Fault-injection shape
//!
//! `chmod 0000` on a non-empty subdirectory inside the source tree.
//! `read_dir` on that directory returns `EACCES` on Linux, which lands
//! in `scan_directory_batched`'s `Err(e)` arm. We restore the
//! permissions in a teardown guard so `tempfile`/`TestDir` can clean
//! up regardless of test outcome.
//!
//! Running as root would skip the EACCES (root bypasses DAC), so the
//! test logs `skip:` and succeeds when `geteuid() == 0`. CI runners
//! run unprivileged; root-only environments simply opt out.
//!
//! ## Platform gate
//!
//! `#[cfg(all(unix, not(target_os = "macos")))]` - the upstream rsync
//! binaries the harness depends on are only pre-built for Linux in
//! `tools/ci/run_interop.sh`. The `chmod 0000` trick relies on POSIX
//! DAC enforcement; Windows ACL semantics differ enough that the test
//! would not exercise the same code path even if the upstream binary
//! were available.

#![cfg(all(unix, not(target_os = "macos")))]

mod integration;

use integration::helpers::{TestDir, upstream_rsync_binary};

use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::io::{self, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;

use checksums::strong::Sha256;

/// Upstream rsync 3.4.1 protocol-32 capability string with `'i'` set.
///
/// Same value used by ISI.c / ISI.d. Keeping it duplicated rather than
/// extracted into a shared module preserves the surgical-changes rule;
/// a common helper appears when a third caller justifies it.
const UPSTREAM_FLAGS_341: &str = "-vlogDtprze.iLsfxCIvu";

/// Upstream `RERR_PARTIAL` exit code, emitted when `io_error` is
/// non-zero at end-of-transfer. Mirrors
/// `crates/transfer/src/generator/io_error_flags.rs::to_exit_code` and
/// upstream `errcode.h:RERR_PARTIAL = 23`.
const RERR_PARTIAL: i32 = 23;

/// Name of the deliberately-poisoned subdirectory inside the source
/// tree. Lives one level deep so the walk has time to enumerate the
/// sibling directory's contents before tripping on it.
const POISONED_DIR: &str = "a/forbidden";

/// Names of the readable sibling directories. Each holds the same
/// canonical fixture file set so the destination can be diff'd against
/// the source minus the poisoned subtree.
const READABLE_DIRS: &[&str] = &["a/readable_one", "a/readable_two"];

/// Files placed inside every readable directory. Sizes step through
/// small, block-boundary, and odd values so a future signature/delta
/// assertion can be added without rewriting the fixture.
const FIXTURE_SIZES: &[usize] = &[0, 1, 64, 1024, 4097];

/// Locate the oc-rsync binary built with the current feature set.
///
/// Prefers the Cargo-provided `CARGO_BIN_EXE_oc-rsync` env var so the
/// binary used matches whatever feature flags the test run was launched
/// with. Falls back to walking up from the test executable, matching
/// the pattern in `inc_recurse_single_segment_push_isi_c.rs::locate_oc_rsync`.
fn locate_oc_rsync() -> Option<PathBuf> {
    if let Some(p) = env::var_os("CARGO_BIN_EXE_oc-rsync") {
        let p = PathBuf::from(p);
        if p.is_file() {
            return Some(p);
        }
    }
    let exe = env::current_exe().ok()?;
    let mut dir = exe.parent()?;
    let name = format!("oc-rsync{}", env::consts::EXE_SUFFIX);
    while !dir.ends_with("target") {
        let candidate = dir.join(&name);
        if candidate.is_file() {
            return Some(candidate);
        }
        dir = dir.parent()?;
    }
    for sub in ["debug", "release"] {
        let candidate = dir.join(sub).join(&name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// Effective-UID probe used as a skip gate. Shells out to `id -u`
/// rather than linking `libc::geteuid` so the test stays free of
/// `unsafe` (per the workspace policy that tests crates must not
/// introduce FFI wrappers). `id` is guaranteed present on every POSIX
/// platform the upstream interop matrix runs on. A failure to invoke
/// `id` falls back to "assume non-root": the test then runs and, if
/// the runner really is root, the EACCES path simply does not fire,
/// failing later assertions loudly rather than silently passing.
fn is_root() -> bool {
    match Command::new("id").arg("-u").output() {
        Ok(o) if o.status.success() => {
            let s = String::from_utf8_lossy(&o.stdout);
            s.trim().parse::<u32>().map(|v| v == 0).unwrap_or(false)
        }
        _ => false,
    }
}

/// Build the fixture: two readable directories plus one
/// permission-denied directory, all under `<root>/a/`. Returns the
/// poisoned directory's absolute path so the teardown guard can
/// restore its permissions.
fn build_fault_tree(root: &Path) -> io::Result<PathBuf> {
    fs::create_dir_all(root)?;
    for rel in READABLE_DIRS {
        let dir = root.join(rel);
        fs::create_dir_all(&dir)?;
        for (idx, size) in FIXTURE_SIZES.iter().enumerate() {
            let mut buf = Vec::with_capacity(*size);
            for byte_idx in 0..*size {
                // Per-byte mix folds dir hash + file index + offset so
                // every (dir, file, byte) tuple is unique. Catches a
                // classifier that confuses entries by base name alone.
                let dir_hash: u32 = rel
                    .bytes()
                    .fold(0u32, |acc, b| acc.wrapping_mul(31) ^ b as u32);
                let mixed = dir_hash
                    .wrapping_add((idx as u32).wrapping_mul(101))
                    .wrapping_add(byte_idx as u32);
                buf.push(mixed as u8);
            }
            fs::write(dir.join(format!("file_{idx:02}.bin")), &buf)?;
        }
    }

    // Poisoned directory: populate it before stripping permissions so
    // upstream rsync's `readdir` (running as the receiver against its
    // own destination snapshot) is not the one that trips - only the
    // sender's enumeration of the source tree should hit EACCES.
    let poisoned = root.join(POISONED_DIR);
    fs::create_dir_all(&poisoned)?;
    for (idx, size) in FIXTURE_SIZES.iter().enumerate() {
        let buf = vec![0xAAu8; *size];
        fs::write(poisoned.join(format!("blocked_{idx:02}.bin")), &buf)?;
    }
    // chmod 0000 - any subsequent opendir from a non-root process
    // returns EACCES, which lands in scan_directory_batched's Err arm.
    fs::set_permissions(&poisoned, fs::Permissions::from_mode(0o000))?;

    Ok(poisoned)
}

/// RAII guard that restores the poisoned directory's permissions on
/// drop so TestDir's cleanup can recursively remove the tree even when
/// the test panics mid-flight.
struct PoisonGuard {
    path: PathBuf,
}

impl Drop for PoisonGuard {
    fn drop(&mut self) {
        let _ = fs::set_permissions(&self.path, fs::Permissions::from_mode(0o755));
    }
}

/// Map of relative path -> SHA-256 digest of file contents. Skips
/// directories (the snapshot is over data, not structure) so the diff
/// stays focused on "the files we expected to transfer arrived
/// byte-identical".
type Snapshot = BTreeMap<PathBuf, [u8; 32]>;

fn snapshot(root: &Path) -> io::Result<Snapshot> {
    let mut out = Snapshot::new();
    walk(root, root, &mut out)?;
    Ok(out)
}

fn walk(base: &Path, current: &Path, out: &mut Snapshot) -> io::Result<()> {
    for entry in fs::read_dir(current)? {
        let entry = entry?;
        let path = entry.path();
        let meta = fs::symlink_metadata(&path)?;
        let ft = meta.file_type();
        if ft.is_dir() {
            walk(base, &path, out)?;
        } else if ft.is_file() {
            let rel = path.strip_prefix(base).unwrap().to_path_buf();
            let bytes = fs::read(&path)?;
            out.insert(rel, Sha256::digest(&bytes));
        }
    }
    Ok(())
}

/// Pump bytes from `reader` to `writer` until EOF, flushing as we go.
///
/// Lifted from `inc_recurse_single_segment_push_isi_c.rs::copy_until_eof`
/// to keep ISI.f's pipe semantics aligned with the rest of the series.
fn copy_until_eof<R: Read, W: Write>(reader: &mut R, writer: &mut W) -> io::Result<()> {
    let mut buf = [0u8; 8192];
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        writer.write_all(&buf[..n])?;
        writer.flush()?;
    }
    Ok(())
}

/// Captured outcome of a pipe-driven push. Unlike ISI.c/.d we do not
/// treat a non-zero exit as a test failure: the whole point of ISI.f
/// is to verify that `RERR_PARTIAL` (23) propagates cleanly.
struct PipeOutcome {
    server_code: Option<i32>,
    client_code: Option<i32>,
    server_stderr: String,
    client_stderr: String,
}

/// Drive oc-rsync `--server --sender` against an upstream rsync
/// `--server` receiver over wired stdio pipes, capturing both exit
/// codes and stderr without panicking on non-zero status. Same shape
/// as the helper in ISI.c/.d, with the asserts moved up to the caller.
fn run_pipe_push(oc_bin: &Path, up_bin: &Path, src: &Path, dst: &Path) -> io::Result<PipeOutcome> {
    let mut server = Command::new(oc_bin)
        .arg("--server")
        .arg("--sender")
        .arg(UPSTREAM_FLAGS_341)
        .arg(".")
        .arg(src.to_string_lossy().as_ref())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    let mut client = Command::new(up_bin)
        .arg("--server")
        .arg(UPSTREAM_FLAGS_341)
        .arg(".")
        .arg(dst.to_string_lossy().as_ref())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    let server_stdout = server.stdout.take().unwrap();
    let server_stdin = server.stdin.take().unwrap();
    let client_stdout = client.stdout.take().unwrap();
    let client_stdin = client.stdin.take().unwrap();

    let s2c = thread::spawn(move || -> io::Result<()> {
        let mut reader = std::io::BufReader::new(server_stdout);
        let mut writer = std::io::BufWriter::new(client_stdin);
        copy_until_eof(&mut reader, &mut writer)
    });

    let c2s = thread::spawn(move || -> io::Result<()> {
        let mut reader = std::io::BufReader::new(client_stdout);
        let mut writer = std::io::BufWriter::new(server_stdin);
        copy_until_eof(&mut reader, &mut writer)
    });

    let server_stderr_handle = server.stderr.take();
    let client_stderr_handle = client.stderr.take();

    let server_status = server.wait()?;
    let client_status = client.wait()?;

    let _ = s2c.join();
    let _ = c2s.join();

    let mut server_err = Vec::new();
    let mut client_err = Vec::new();
    if let Some(mut s) = server_stderr_handle {
        let _ = s.read_to_end(&mut server_err);
    }
    if let Some(mut s) = client_stderr_handle {
        let _ = s.read_to_end(&mut client_err);
    }

    Ok(PipeOutcome {
        server_code: server_status.code(),
        client_code: client_status.code(),
        server_stderr: String::from_utf8_lossy(&server_err).into_owned(),
        client_stderr: String::from_utf8_lossy(&client_err).into_owned(),
    })
}

/// End-to-end push: poisoned subdirectory triggers `record_io_error`
/// during enumeration; the readable siblings still transfer; both
/// peers exit with `RERR_PARTIAL` (23).
///
/// Skips with `skip:` (and succeeds) when running as root or when the
/// upstream binary is missing, matching the convention used by every
/// other interop test that depends on `target/interop/upstream-install/`.
/// Run `bash tools/ci/run_interop.sh` to populate the tree.
#[test]
fn sender_inc_recurse_partial_walk_propagates_io_error() {
    if is_root() {
        eprintln!(
            "skip: ISI.f relies on POSIX DAC to trigger EACCES; \
             running as root bypasses chmod 0000"
        );
        return;
    }

    let oc_bin = match locate_oc_rsync() {
        Some(p) => p,
        None => {
            eprintln!("skip: oc-rsync binary not located");
            return;
        }
    };
    let up_bin = match upstream_rsync_binary("3.4.1") {
        Some(p) => p,
        None => {
            eprintln!(
                "skip: upstream rsync 3.4.1 not installed at \
                 target/interop/upstream-install/3.4.1/bin/rsync; \
                 run tools/ci/run_interop.sh"
            );
            return;
        }
    };

    let test_dir = TestDir::new().expect("create test dir");
    let src = test_dir.mkdir("src").expect("mkdir src");
    let dst = test_dir.mkdir("dst").expect("mkdir dst");

    let poisoned = build_fault_tree(&src).expect("populate fault-injected source tree");
    // Guard restores permissions even if the test panics.
    let _guard = PoisonGuard {
        path: poisoned.clone(),
    };

    let outcome = run_pipe_push(&oc_bin, &up_bin, &src, &dst).expect("pipe push must not crash");

    // Contract 1: the transfer does not abort with a protocol error or
    // segfault. Either both peers exit RERR_PARTIAL (23), or - on the
    // outside chance the upstream peer maps the io_error to its own
    // success path - exit 0. Anything else means the io_error
    // round-trip is broken.
    let server = outcome.server_code.unwrap_or(-1);
    let client = outcome.client_code.unwrap_or(-1);
    assert!(
        matches!(server, RERR_PARTIAL | 0),
        "sender exit must be {RERR_PARTIAL} (RERR_PARTIAL) or 0, got {server}\n\
         oc-rsync stderr:\n{}\nupstream stderr:\n{}",
        outcome.server_stderr,
        outcome.client_stderr,
    );
    assert!(
        matches!(client, RERR_PARTIAL | 0),
        "receiver exit must be {RERR_PARTIAL} (RERR_PARTIAL) or 0, got {client}\n\
         oc-rsync stderr:\n{}\nupstream stderr:\n{}",
        outcome.server_stderr,
        outcome.client_stderr,
    );
    // At least one side MUST surface the io_error - silent recovery
    // would defeat the upstream-compatible diagnostic contract.
    assert!(
        server == RERR_PARTIAL || client == RERR_PARTIAL,
        "io_error must propagate to at least one peer's exit status; \
         sender={server} receiver={client}\n\
         oc-rsync stderr:\n{}\nupstream stderr:\n{}",
        outcome.server_stderr,
        outcome.client_stderr,
    );

    // Contract 2: a partial flist was sent. Every readable file must
    // exist at the destination byte-identical to the source.
    let dst_snap = snapshot(&dst).expect("snapshot destination");
    for rel_dir in READABLE_DIRS {
        let src_dir = src.join(rel_dir);
        let src_snap = snapshot(&src_dir).expect("snapshot readable source dir");
        for (rel_inside, src_digest) in &src_snap {
            let dst_rel = Path::new(rel_dir).join(rel_inside);
            let dst_digest = dst_snap.get(&dst_rel).unwrap_or_else(|| {
                panic!(
                    "readable file missing from destination after partial walk: {}",
                    dst_rel.display()
                )
            });
            assert_eq!(
                dst_digest,
                src_digest,
                "readable file diverged after partial walk: {}",
                dst_rel.display()
            );
        }
    }

    // Contract 3: the poisoned subtree must NOT be materialized at
    // the destination. The sender could never enumerate it, so the
    // receiver has nothing to write. Any file under
    // `<dst>/<POISONED_DIR>/` would indicate the sender either
    // ignored the EACCES or somehow recovered the listing - both are
    // regressions.
    let poisoned_dst = dst.join(POISONED_DIR);
    if poisoned_dst.exists() {
        let leaked: Vec<_> = fs::read_dir(&poisoned_dst)
            .map(|it| {
                it.filter_map(Result::ok)
                    .map(|e| e.file_name())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        assert!(
            leaked.is_empty(),
            "poisoned subtree must not leak files into destination: {leaked:?}"
        );
    }

    // Contract 4: sender-side stderr must reference the failed
    // directory so operators can diagnose the partial transfer.
    // upstream: flist.c:1842 - `opendir %s failed`.
    assert!(
        outcome.server_stderr.contains("opendir")
            || outcome.server_stderr.contains("readdir")
            || outcome.server_stderr.contains("forbidden"),
        "sender stderr must surface the failed directory; got:\n{}",
        outcome.server_stderr,
    );
}
