//! ISI.c - single-segment INC_RECURSE push interop.
//!
//! Smallest possible end-to-end exercise of the sender-side INC_RECURSE
//! path: an oc-rsync `--server --sender` instance (INC_RECURSE is
//! unconditional since ISI.h) pushes a 10-file single-directory tree to
//! an upstream rsync `--server` receiver over wired pipes. The
//! destination must match the source byte-for-byte and the capability
//! string we would emit must include `'i'`.
//!
//! ## Why "single-segment"
//!
//! Upstream `flist.c:46` defines `MIN_FILECNT_LOOKAHEAD = 1000`. With
//! ten files in one flat directory the partitioning logic in
//! `crates/transfer/src/generator/file_list/inc_recurse.rs` produces a
//! single initial segment, no sub-list dispatch, and the `NDX_FLIST_EOF`
//! marker fires immediately on the first scheduler tick. This is the
//! narrowest INC_RECURSE shape that still exercises:
//!
//! - the capability-bit advertisement at
//!   `crates/transfer/src/setup/capability.rs::build_capability_string`,
//! - `GeneratorContext::inc_recurse()` returning `true` post-handshake,
//! - `partition_file_list_for_inc_recurse` populating
//!   `initial_segment_count`,
//! - `SegmentScheduler::is_exhausted` -> `send_flist_eof` on the very
//!   first iteration of the transfer loop.
//!
//! ISI.d expands this to multi-segment trees, ISI.e adds wire-byte
//! parity assertions against upstream tcpdump captures, ISI.f checks
//! `parent_dir_ndx` alignment on adversarial fixtures. This file is the
//! regression seed all three build on.
//!
//! ## Platform gate
//!
//! `#[cfg(all(unix, not(target_os = "macos")))]` - the upstream rsync
//! binaries the harness depends on are only pre-built for Linux in
//! `tools/ci/run_interop.sh`. macOS has no `target/interop/upstream-install`
//! tree in standard CI, so the test would be a perpetual skip there;
//! gating at compile time keeps the cfg surface honest.
//!
//! ## Upstream availability
//!
//! Looked up via `integration::helpers::upstream_rsync_binary("3.4.1")`.
//! If `target/interop/upstream-install/3.4.1/bin/rsync` is missing the
//! test logs `skip:` and returns successfully - it does not fail purely
//! because the binary is absent. Run `bash tools/ci/run_interop.sh` to
//! populate the install tree.
//!
//! ## ISI.e seed
//!
//! The pipe driver isolates the sender's stdout stream so a future
//! wire-byte capture can be added incrementally: tee `server_stdout` to
//! a buffer in `run_pipe_push`, decode `MSG_DATA` frames, and assert the
//! `NDX_FLIST` / `NDX_FLIST_EOF` sequence matches the golden trace.

#![cfg(all(unix, not(target_os = "macos")))]

mod integration;

use integration::helpers::{TestDir, upstream_rsync_binary};

use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;

use checksums::strong::Sha256;
use core::client::ClientConfig;
use transfer::setup::build_capability_string;

/// Upstream rsync 3.4.1 protocol-32 capability string with `'i'` set.
///
/// Sender sends this when it advertises INC_RECURSE to the receiver.
/// Mirrors `flags_for_version("3.4.1")` in the sister fuzz harness.
const UPSTREAM_FLAGS_341: &str = "-vlogDtprze.iLsfxCIvu";

/// Locate the oc-rsync binary built with the current feature set.
///
/// Prefers the Cargo-provided `CARGO_BIN_EXE_oc-rsync` env var so the
/// binary used matches whatever feature flags the test run was launched
/// with. Falls back to walking up from the test executable, matching
/// the pattern in `inc_recurse_sender_fuzz_1863.rs::locate_oc_rsync`.
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

/// Deterministic 10-file flat tree.
///
/// All files live directly under `root`. Sizes vary across small,
/// block-boundary, and odd values so a future signature/delta assertion
/// can be added without rewriting the fixture. Contents are fully
/// deterministic so the source-snapshot hash is stable across runs.
fn build_single_segment_tree(root: &Path) -> io::Result<()> {
    fs::create_dir_all(root)?;
    let sizes = [0usize, 1, 7, 64, 256, 1024, 4096, 4097, 8192, 12345];
    for (idx, size) in sizes.iter().enumerate() {
        let name = format!("file_{idx:02}.bin");
        let mut buf = Vec::with_capacity(*size);
        for byte_idx in 0..*size {
            // Pattern is index-dependent so two different files of the
            // same size still differ. Cheap and deterministic.
            buf.push(((idx as u32).wrapping_mul(31) ^ byte_idx as u32) as u8);
        }
        fs::write(root.join(&name), &buf)?;
    }
    Ok(())
}

/// Map of relative path -> SHA-256 digest of file contents.
///
/// Directory entries are not represented: the single-segment fixture is
/// flat by construction, so any directory in the destination snapshot
/// other than the destination root itself indicates a transfer bug.
type Snapshot = BTreeMap<PathBuf, [u8; 32]>;

fn snapshot(root: &Path) -> io::Result<Snapshot> {
    let mut out = Snapshot::new();
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        let meta = fs::symlink_metadata(&path)?;
        if !meta.file_type().is_file() {
            return Err(io::Error::other(format!(
                "single-segment fixture must be flat; saw non-file entry {}",
                path.display()
            )));
        }
        let rel = path.strip_prefix(root).unwrap().to_path_buf();
        let bytes = fs::read(&path)?;
        out.insert(rel, Sha256::digest(&bytes));
    }
    Ok(out)
}

fn diff_snapshots(src: &Snapshot, dst: &Snapshot) -> Vec<String> {
    let mut errors = Vec::new();
    for (p, sd) in src {
        match dst.get(p) {
            None => errors.push(format!("missing in dst: {}", p.display())),
            Some(dd) if dd != sd => errors.push(format!("digest mismatch: {}", p.display())),
            _ => {}
        }
    }
    for p in dst.keys() {
        if !src.contains_key(p) {
            errors.push(format!("extra in dst: {}", p.display()));
        }
    }
    errors
}

/// Pump bytes from `reader` to `writer` until EOF, flushing as we go.
///
/// Lifted verbatim from `inc_recurse_sender_fuzz_1863::copy_until_eof`
/// so the two harnesses share their pipe semantics.
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

/// Drive the oc-rsync sender against an upstream rsync receiver via
/// wired stdio pipes. Mirrors the SSH transport without needing sshd.
///
/// The oc-rsync side is launched **without** `--inc-recursive` on the
/// CLI: the builder default (unconditional since ISI.h) already
/// advertises the `'i'` bit on push transfers. That is the precise
/// invariant ISI.c locks in.
fn run_pipe_push(oc_bin: &Path, up_bin: &Path, src: &Path, dst: &Path) -> io::Result<()> {
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

    let server_stderr = server.stderr.take();
    let client_stderr = client.stderr.take();

    let server_status = server.wait()?;
    let client_status = client.wait()?;

    let _ = s2c.join();
    let _ = c2s.join();

    if !server_status.success() || !client_status.success() {
        let mut server_err = Vec::new();
        let mut client_err = Vec::new();
        if let Some(mut s) = server_stderr {
            let _ = s.read_to_end(&mut server_err);
        }
        if let Some(mut s) = client_stderr {
            let _ = s.read_to_end(&mut client_err);
        }
        return Err(io::Error::other(format!(
            "pipe push failed: oc-rsync server status={:?} upstream client status={:?}\n\
             oc-rsync stderr:\n{}\nupstream stderr:\n{}",
            server_status.code(),
            client_status.code(),
            String::from_utf8_lossy(&server_err),
            String::from_utf8_lossy(&client_err),
        )));
    }
    Ok(())
}

/// Default `ClientConfig` must advertise INC_RECURSE unconditionally.
///
/// This is the in-process counterpart to the byte-level pipe push: if
/// the builder default ever regresses to `false` the capability string
/// stops carrying `'i'` and the upstream peer silently falls back to
/// non-INC_RECURSE mode. Catching that here saves debugging a wire
/// divergence later in the chain.
#[test]
fn default_capability_string_includes_inc_recurse() {
    let config = ClientConfig::builder().build();
    assert!(
        config.inc_recursive_send(),
        "inc_recursive_send default must be ON"
    );

    let caps = build_capability_string(config.inc_recursive_send());
    assert!(
        caps.starts_with("-e."),
        "capability string must keep the -e. prefix: {caps}"
    );
    assert!(
        caps.contains('i'),
        "capability string must include 'i' by default: {caps}"
    );
}

/// `--no-inc-recursive` must still suppress `'i'` even with the default ON.
///
/// Mirrors the upstream `set_allow_inc_recurse()` precedent: the CLI
/// override wins over the compiled-in default.
#[test]
fn no_inc_recursive_override_suppresses_inc_recurse_bit() {
    let config = ClientConfig::builder().inc_recursive_send(false).build();
    assert!(!config.inc_recursive_send());
    let caps = build_capability_string(config.inc_recursive_send());
    assert!(
        !caps.contains('i'),
        "--no-inc-recursive override must drop 'i' even with the default ON: {caps}"
    );
}

/// End-to-end push: 10 files, single directory, byte-identical destination.
///
/// Skips with `skip:` (and succeeds) when the upstream binary is not
/// available, matching the convention used by every other interop test
/// that depends on `target/interop/upstream-install/`. Run
/// `bash tools/ci/run_interop.sh` to populate the tree.
#[test]
fn single_segment_push_to_upstream_3_4_1_byte_identical() {
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

    build_single_segment_tree(&src).expect("populate source tree");
    let src_snap = snapshot(&src).expect("snapshot source");
    assert_eq!(
        src_snap.len(),
        10,
        "single-segment fixture must produce exactly 10 files"
    );

    run_pipe_push(&oc_bin, &up_bin, &src, &dst).expect("pipe push must succeed");

    let dst_snap = snapshot(&dst).expect("snapshot destination");
    let diffs = diff_snapshots(&src_snap, &dst_snap);
    assert!(
        diffs.is_empty(),
        "destination diverged from source after INC_RECURSE push:\n{}",
        diffs.join("\n")
    );
}
