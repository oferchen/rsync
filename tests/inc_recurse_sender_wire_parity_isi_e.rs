//! ISI.e - wire-byte parity: oc-rsync sender vs upstream rsync sender.
//!
//! ISI.c (single-segment push, PR #4842) and ISI.d (multi-segment push,
//! PR #4846) proved an oc-rsync `--server --sender` instance can drive
//! an upstream 3.4.1 receiver to a byte-identical destination tree.
//! Both deferred the stronger assertion - byte-by-byte parity on the
//! sender's outbound wire stream - to this file (`TODO: ISI.e asserts
//! wire-byte parity`).
//!
//! ## What this proves
//!
//! For the same source fixture, the same protocol version, the same
//! capability string, and a fixed `--checksum-seed`, the bytes the
//! oc-rsync sender writes to stdout must be identical to the bytes an
//! upstream rsync 3.4.1 sender writes to stdout under the same
//! conditions. Any drift - a varint-encoding mismatch, a flist-sort
//! ordering divergence, an off-by-one in xattr advertisement, a stale
//! capability bit - manifests as a byte difference at a deterministic
//! offset and the test fails with a hex dump pointing at the first
//! divergent position.
//!
//! ## Why this is `#[ignore]` today
//!
//! oc-rsync's sender wire output is not yet bit-identical to upstream's.
//! Per the ISI.e task spec the goal of this file is to *detect* that
//! drift, not to gate CI on it. Once ISI.f closes the divergences this
//! gate can be flipped to a regular `#[test]`. Removing `#[ignore]`
//! prematurely would mask the divergences these assertions exist to
//! catch.
//!
//! ## Determinism levers
//!
//! Three deliberate knobs eliminate noise that would otherwise mask
//! genuine protocol drift:
//!
//! 1. **Fixed mtimes.** Every fixture file is `set_file_times`'d to a
//!    constant `(secs, nsecs)` so the flist's encoded mtime bytes match
//!    across runs. Without this the per-second wall-clock at fixture
//!    creation leaks into the wire stream.
//! 2. **Fixed checksum seed.** `--checksum-seed=12345` is forwarded to
//!    the upstream receiver, which writes the seed to the sender during
//!    `setup_protocol`. The sender then uses it for every strong file
//!    checksum it emits. `--checksum-seed=0` would silently fall back
//!    to `time(NULL) XOR getpid()` (see
//!    `setup_protocol_server_seed_zero_uses_time_based_generation` in
//!    `crates/transfer/src/setup/tests.rs`), so a non-zero value is
//!    mandatory.
//! 3. **No `-v`.** Verbose mode emits `MSG_INFO` frames with free-form
//!    English text ("sent X bytes, received Y bytes") whose exact
//!    wording differs between implementations and contains
//!    transfer-rate floats that vary per run. The capability string
//!    `-logDtprze.iLsfxCIvu` drops the leading `v` (CLI verbose) while
//!    keeping `v` (protocol-level varint flist flags advertisement)
//!    inside the post-`.` capability segment.
//!
//! ## Platform gate
//!
//! `#[cfg(all(unix, not(target_os = "macos")))]` - identical reasoning
//! to ISI.c/ISI.d. The upstream rsync binaries the harness depends on
//! are pre-built only for Linux in `tools/ci/run_interop.sh`; macOS
//! would be a perpetual skip.

#![cfg(all(unix, not(target_os = "macos")))]

mod integration;

use integration::helpers::{TestDir, upstream_rsync_binary};

use std::env;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;

use filetime::{FileTime, set_file_times};

/// Upstream rsync 3.4.1 capability string with `'i'` (INC_RECURSE) set
/// and no leading CLI `-v` (verbose). See module docs for why `v` is
/// dropped: it injects free-form transfer-rate strings that vary per
/// run and mask genuine wire-protocol drift.
const PARITY_FLAGS_341: &str = "-logDtprze.iLsfxCIvu";

/// Fixed checksum seed forwarded to the upstream receiver. Must be
/// non-zero - `--checksum-seed=0` is treated as "generate from time XOR
/// PID" by upstream rsync (`setup_protocol_server_seed_zero_uses_time_based_generation`).
const FIXED_CHECKSUM_SEED: &str = "--checksum-seed=12345";

/// Fixed mtime applied to every fixture file. Picked far enough in the
/// past that the upstream rsync receiver's mtime-encoding path takes
/// the post-2038 negative-seconds branch nowhere near this value, and
/// recent enough that varint encoding stays compact.
const FIXTURE_MTIME_SECS: i64 = 1_700_000_000;
const FIXTURE_MTIME_NSECS: u32 = 0;

/// Locate the oc-rsync binary built with the current feature set.
/// Same shape as ISI.c/ISI.d.
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

/// Apply the fixed mtime to every regular file in `root` recursively.
///
/// Walks the tree post-fixture-build so the fixture-build code stays
/// identical to ISI.c/ISI.d. Directory mtimes are also stamped: the
/// upstream rsync receiver encodes parent-directory mtimes into the
/// flist segment headers, so leaving them at fresh-create wall-clock
/// would defeat the parity assertion.
fn stamp_fixed_mtimes(root: &Path) -> io::Result<()> {
    let mtime = FileTime::from_unix_time(FIXTURE_MTIME_SECS, FIXTURE_MTIME_NSECS);
    fn recurse(dir: &Path, mtime: FileTime) -> io::Result<()> {
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            let meta = fs::symlink_metadata(&path)?;
            if meta.file_type().is_dir() {
                recurse(&path, mtime)?;
            }
            set_file_times(&path, mtime, mtime)?;
        }
        Ok(())
    }
    recurse(root, mtime)?;
    set_file_times(root, mtime, mtime)?;
    Ok(())
}

/// Deterministic 10-file flat tree, mirroring ISI.c's fixture.
fn build_single_segment_tree(root: &Path) -> io::Result<()> {
    fs::create_dir_all(root)?;
    let sizes = [0usize, 1, 7, 64, 256, 1024, 4096, 4097, 8192, 12345];
    for (idx, size) in sizes.iter().enumerate() {
        let name = format!("file_{idx:02}.bin");
        let mut buf = Vec::with_capacity(*size);
        for byte_idx in 0..*size {
            buf.push(((idx as u32).wrapping_mul(31) ^ byte_idx as u32) as u8);
        }
        fs::write(root.join(&name), &buf)?;
    }
    stamp_fixed_mtimes(root)
}

/// Deterministic deep tree, mirroring ISI.d's fixture.
fn build_multi_segment_tree(root: &Path) -> io::Result<()> {
    const LAYOUT: &[(&str, usize)] = &[("a/b/c", 10), ("a/b/d", 10), ("a/e/f", 10)];
    fs::create_dir_all(root)?;
    let sizes = [0usize, 1, 7, 64, 256, 1024, 4096, 4097, 8192, 12345];
    for (dir_idx, (rel_dir, count)) in LAYOUT.iter().enumerate() {
        assert_eq!(*count, sizes.len());
        let dir_path = root.join(rel_dir);
        fs::create_dir_all(&dir_path)?;
        for (file_idx, size) in sizes.iter().enumerate() {
            let name = format!("file_{file_idx:02}.bin");
            let mut buf = Vec::with_capacity(*size);
            for byte_idx in 0..*size {
                let mixed = (dir_idx as u32)
                    .wrapping_mul(1009)
                    .wrapping_add((file_idx as u32).wrapping_mul(31))
                    .wrapping_add(byte_idx as u32);
                buf.push(mixed as u8);
            }
            fs::write(dir_path.join(&name), &buf)?;
        }
    }
    stamp_fixed_mtimes(root)
}

/// Pump bytes from `reader` to `writer`, optionally teeing every chunk
/// into `tap`. Flushes `writer` after each chunk so the downstream peer
/// sees data promptly - critical for the receiver, which holds the
/// transfer back until it has received and parsed each flist segment.
fn copy_and_tap<R: Read, W: Write>(
    reader: &mut R,
    writer: &mut W,
    tap: Option<Arc<Mutex<Vec<u8>>>>,
) -> io::Result<()> {
    let mut buf = [0u8; 8192];
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        if let Some(t) = &tap {
            t.lock().unwrap().extend_from_slice(&buf[..n]);
        }
        writer.write_all(&buf[..n])?;
        writer.flush()?;
    }
    Ok(())
}

/// Drive a sender against an upstream receiver over wired pipes,
/// returning the exact byte sequence the sender wrote to stdout (the
/// sender->receiver wire stream).
///
/// `sender_bin` is the sender process; `receiver_bin` is always the
/// upstream rsync binary acting as `--server` (receiver). Holding the
/// receiver constant across both runs of a parity test means the only
/// variable is the sender implementation: any byte divergence is
/// attributable to the sender, not the receiver.
fn capture_sender_wire(
    sender_bin: &Path,
    receiver_bin: &Path,
    src: &Path,
    dst: &Path,
) -> io::Result<Vec<u8>> {
    let mut sender = Command::new(sender_bin)
        .arg("--server")
        .arg("--sender")
        .arg(FIXED_CHECKSUM_SEED)
        .arg(PARITY_FLAGS_341)
        .arg(".")
        .arg(src.to_string_lossy().as_ref())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    let mut receiver = Command::new(receiver_bin)
        .arg("--server")
        .arg(FIXED_CHECKSUM_SEED)
        .arg(PARITY_FLAGS_341)
        .arg(".")
        .arg(dst.to_string_lossy().as_ref())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    let sender_stdout = sender.stdout.take().unwrap();
    let sender_stdin = sender.stdin.take().unwrap();
    let receiver_stdout = receiver.stdout.take().unwrap();
    let receiver_stdin = receiver.stdin.take().unwrap();

    let s2r_tap = Arc::new(Mutex::new(Vec::new()));
    let s2r_tap_thread = Arc::clone(&s2r_tap);

    let s2r = thread::spawn(move || -> io::Result<()> {
        let mut reader = std::io::BufReader::new(sender_stdout);
        let mut writer = std::io::BufWriter::new(receiver_stdin);
        copy_and_tap(&mut reader, &mut writer, Some(s2r_tap_thread))
    });

    let r2s = thread::spawn(move || -> io::Result<()> {
        let mut reader = std::io::BufReader::new(receiver_stdout);
        let mut writer = std::io::BufWriter::new(sender_stdin);
        copy_and_tap(&mut reader, &mut writer, None)
    });

    let sender_stderr = sender.stderr.take();
    let receiver_stderr = receiver.stderr.take();

    let sender_status = sender.wait()?;
    let receiver_status = receiver.wait()?;

    let _ = s2r.join();
    let _ = r2s.join();

    if !sender_status.success() || !receiver_status.success() {
        let mut sender_err = Vec::new();
        let mut receiver_err = Vec::new();
        if let Some(mut s) = sender_stderr {
            let _ = s.read_to_end(&mut sender_err);
        }
        if let Some(mut s) = receiver_stderr {
            let _ = s.read_to_end(&mut receiver_err);
        }
        return Err(io::Error::other(format!(
            "wire capture failed: sender status={:?} receiver status={:?}\n\
             sender stderr:\n{}\nreceiver stderr:\n{}",
            sender_status.code(),
            receiver_status.code(),
            String::from_utf8_lossy(&sender_err),
            String::from_utf8_lossy(&receiver_err),
        )));
    }

    let captured = Arc::try_unwrap(s2r_tap)
        .expect("tap arc must be uniquely owned after threads join")
        .into_inner()
        .expect("tap mutex must not be poisoned");
    Ok(captured)
}

/// Format the first 256 bytes of `bytes` as a hex+ASCII dump suitable
/// for a panic message. 16 bytes per line, offset in the left margin.
fn hex_dump_head(bytes: &[u8], limit: usize) -> String {
    use std::fmt::Write;
    let n = bytes.len().min(limit);
    let mut out = String::new();
    for (i, chunk) in bytes[..n].chunks(16).enumerate() {
        let _ = write!(out, "{:08x}  ", i * 16);
        for j in 0..16 {
            if j < chunk.len() {
                let _ = write!(out, "{:02x} ", chunk[j]);
            } else {
                out.push_str("   ");
            }
            if j == 7 {
                out.push(' ');
            }
        }
        out.push_str(" |");
        for &b in chunk {
            out.push(if (0x20..0x7f).contains(&b) {
                b as char
            } else {
                '.'
            });
        }
        out.push_str("|\n");
    }
    out
}

/// Compare two byte streams. On mismatch panic with a detailed report
/// pointing at the first divergent offset and dumping context from
/// both streams around that point.
fn assert_wire_parity(label: &str, oc_bytes: &[u8], upstream_bytes: &[u8]) {
    if oc_bytes == upstream_bytes {
        return;
    }

    let first_diff = oc_bytes
        .iter()
        .zip(upstream_bytes.iter())
        .position(|(a, b)| a != b)
        .unwrap_or_else(|| oc_bytes.len().min(upstream_bytes.len()));

    let window_start = first_diff.saturating_sub(16);
    let window_end_oc = (first_diff + 64).min(oc_bytes.len());
    let window_end_up = (first_diff + 64).min(upstream_bytes.len());

    panic!(
        "[{label}] sender wire bytes diverge from upstream rsync sender\n\
         oc-rsync capture length:  {oc_len}\n\
         upstream capture length:  {up_len}\n\
         first divergence offset:  0x{first_diff:08x} ({first_diff})\n\
         \n\
         --- oc-rsync (first 256 bytes) ---\n{oc_head}\n\
         --- upstream (first 256 bytes) ---\n{up_head}\n\
         --- oc-rsync window around divergence (offset 0x{ws:08x}..0x{we_oc:08x}) ---\n{oc_win}\n\
         --- upstream window around divergence (offset 0x{ws:08x}..0x{we_up:08x}) ---\n{up_win}\n",
        label = label,
        oc_len = oc_bytes.len(),
        up_len = upstream_bytes.len(),
        first_diff = first_diff,
        oc_head = hex_dump_head(oc_bytes, 256),
        up_head = hex_dump_head(upstream_bytes, 256),
        ws = window_start,
        we_oc = window_end_oc,
        we_up = window_end_up,
        oc_win = hex_dump_head(&oc_bytes[window_start..window_end_oc], 256),
        up_win = hex_dump_head(&upstream_bytes[window_start..window_end_up], 256),
    );
}

/// Set up the binary handles shared by every parity test. Returns
/// `None` (and emits a `skip:` line) when either binary is missing,
/// matching the convention used by every other interop test under
/// `tests/`.
fn locate_binaries_or_skip() -> Option<(PathBuf, PathBuf)> {
    let oc_bin = match locate_oc_rsync() {
        Some(p) => p,
        None => {
            eprintln!("skip: oc-rsync binary not located");
            return None;
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
            return None;
        }
    };
    Some((oc_bin, up_bin))
}

/// Single-segment fixture: 10 files in one flat directory.
///
/// `#[ignore]` because oc-rsync's sender wire output is not yet
/// bit-identical to upstream's. ISI.f will close the divergences; once
/// it does, remove `#[ignore]` so CI gates on parity.
#[test]
#[ignore = "ISI.f tracks fixing wire-byte divergences; this test detects them"]
fn single_segment_sender_wire_byte_parity_3_4_1() {
    let (oc_bin, up_bin) = match locate_binaries_or_skip() {
        Some(pair) => pair,
        None => return,
    };

    let oc_run = TestDir::new().expect("create oc-rsync test dir");
    let oc_src = oc_run.mkdir("src").expect("mkdir oc src");
    let oc_dst = oc_run.mkdir("dst").expect("mkdir oc dst");
    build_single_segment_tree(&oc_src).expect("populate oc source");

    let up_run = TestDir::new().expect("create upstream test dir");
    let up_src = up_run.mkdir("src").expect("mkdir up src");
    let up_dst = up_run.mkdir("dst").expect("mkdir up dst");
    build_single_segment_tree(&up_src).expect("populate upstream source");

    let oc_bytes =
        capture_sender_wire(&oc_bin, &up_bin, &oc_src, &oc_dst).expect("capture oc-rsync sender");
    let up_bytes =
        capture_sender_wire(&up_bin, &up_bin, &up_src, &up_dst).expect("capture upstream sender");

    assert_wire_parity("single-segment", &oc_bytes, &up_bytes);
}

/// Multi-segment fixture: deep tree forcing multiple `NDX_FLIST_OFFSET`
/// sub-list headers, mirroring ISI.d's shape.
///
/// `#[ignore]` for the same reason as the single-segment variant - ISI.f
/// tracks the parity work, this test exists to detect divergences.
#[test]
#[ignore = "ISI.f tracks fixing wire-byte divergences; this test detects them"]
fn multi_segment_sender_wire_byte_parity_3_4_1() {
    let (oc_bin, up_bin) = match locate_binaries_or_skip() {
        Some(pair) => pair,
        None => return,
    };

    let oc_run = TestDir::new().expect("create oc-rsync test dir");
    let oc_src = oc_run.mkdir("src").expect("mkdir oc src");
    let oc_dst = oc_run.mkdir("dst").expect("mkdir oc dst");
    build_multi_segment_tree(&oc_src).expect("populate oc source");

    let up_run = TestDir::new().expect("create upstream test dir");
    let up_src = up_run.mkdir("src").expect("mkdir up src");
    let up_dst = up_run.mkdir("dst").expect("mkdir up dst");
    build_multi_segment_tree(&up_src).expect("populate upstream source");

    let oc_bytes =
        capture_sender_wire(&oc_bin, &up_bin, &oc_src, &oc_dst).expect("capture oc-rsync sender");
    let up_bytes =
        capture_sender_wire(&up_bin, &up_bin, &up_src, &up_dst).expect("capture upstream sender");

    assert_wire_parity("multi-segment", &oc_bytes, &up_bytes);
}
