//! ISI.g - Sender INC_RECURSE start-time win on a large-file-count source.
//!
//! Companion to the ISI.a-f correctness work. Where ISI.c (`#4842`) and
//! ISI.d (`#4846`) proved the sender-side INC_RECURSE pipeline produces a
//! byte-identical destination, and ISI.e (`#4859`) locked in wire-byte
//! parity against upstream's sender output, this bench measures the
//! **start-time win** that INC_RECURSE was invented to deliver in the
//! first place.
//!
//! # The original motivation
//!
//! Upstream rsync's INC_RECURSE design (`flist.c:46 MIN_FILECNT_LOOKAHEAD`,
//! `compat.c:720 set_allow_inc_recurse()`) lets the sender begin
//! transferring file data **before** the full file list has been built.
//! Without it the sender must walk the entire source tree, build every
//! `FileEntry`, and serialize the whole list onto the wire before the
//! receiver can request a single file - an O(N) start-up cost on the
//! number of source entries.
//!
//! With INC_RECURSE the sender emits only the initial top-level segment,
//! then interleaves further segments with the actual file data. First-byte
//! latency drops from O(N) to roughly O(top-level-fanout + log N) for a
//! balanced tree; for a 100 000-file source the theoretical gain is two
//! orders of magnitude.
//!
//! # What the bench measures
//!
//! Three pipe-driven push transfers against the same source tree:
//!
//! | Bench | Sender invocation                             | INC_RECURSE on the wire? |
//! |-------|------------------------------------------------|--------------------------|
//! | A     | `oc-rsync` with `--no-inc-recursive`             | No  (baseline) |
//! | B     | `oc-rsync` with default flags (INC_RECURSE is unconditional since ISI.h) | Yes (under test) |
//! | C     | upstream `rsync` (protocol >= 30, default ON)     | Yes (reference) |
//!
//! Each cell reports two metrics:
//!
//! 1. **Time-to-first-data-bytes** - wall time from spawn to a cumulative
//!    `OC_RSYNC_BENCH_FIRST_DATA_BYTES` (default `1 048 576` bytes) on
//!    `server_stdout`. With INC_RECURSE OFF the sender must finish the
//!    initial flist serialization before the byte counter starts climbing,
//!    so this captures the start-up stall. With INC_RECURSE ON the sender
//!    streams the first segment immediately, so the threshold is reached
//!    after only the small handshake + top-level-segment payload.
//! 2. **Total transfer time** - wall time from spawn to both child
//!    processes exiting cleanly. Reported as a sanity check: the win is
//!    in *first-byte latency*, not steady-state throughput, so the total
//!    transfer times for A and B should converge while the start-time
//!    metric should diverge.
//!
//! The headline number the bench targets is the ratio `A / B` on the
//! first-data-bytes metric, with C as the upstream reference for "how
//! fast should INC_RECURSE feel when it works".
//!
//! # Fixture
//!
//! Default: 100 000 files spread evenly across 100 directories (1 000
//! files per directory), each file a deterministic 16-byte payload. The
//! directory count is small enough that upstream/oc-rsync emit only a
//! handful of sub-list segments (one initial + ~100 follow-ups), so the
//! comparison stays in the regime where INC_RECURSE's "stream the initial
//! segment, defer the rest" behaviour dominates the wall-clock.
//!
//! Per-file payloads are intentionally tiny so the bench wall-clock is
//! dominated by file-list construction and transfer setup rather than
//! delta-codec throughput. This is the regime where INC_RECURSE has its
//! biggest measurable effect.
//!
//! # Escape hatches
//!
//! - `OC_RSYNC_BENCH_SCALE` - override the file count. Accepts a positive
//!   integer; defaults to `100_000`. Useful for quick local sanity runs:
//!   `OC_RSYNC_BENCH_SCALE=10000 cargo bench -p transfer
//!   --bench isi_g_sender_inc_recurse_start_time` runs the same harness
//!   against 10 000 files. The bench preserves the "1 000 files per
//!   directory" ratio when scaling.
//! - `OC_RSYNC_BENCH_SKIP=1` - print a skip message and exit. Useful for
//!   CI environments where the upstream binary is missing or the disk
//!   budget cannot absorb 100 000 inodes.
//! - `OC_RSYNC_BENCH_FIRST_DATA_BYTES` - override the start-time
//!   threshold (default `1_048_576`). Lower it to measure protocol
//!   greeting latency, raise it to require more file-list payload.
//! - `OC_RSYNC_BIN` / `OC_RSYNC_BIN_UPSTREAM` - override the binary paths.
//!   Defaults match the build script convention used by
//!   `pip_6_end_to_end_parallel_vs_sequential`. The baseline and under-test
//!   cells share the same `oc-rsync` binary; only the `--no-inc-recursive`
//!   flag differs between them.
//!
//! # Skip semantics
//!
//! Each bench cell resolves its required binary independently and prints
//! a `skip:` message instead of panicking when the binary is absent. This
//! matches the convention used by every other interop-dependent bench
//! and test (`crates/core/benches/pip_6_end_to_end_parallel_vs_sequential.rs`,
//! `tests/inc_recurse_single_segment_push_isi_c.rs`). The bench is
//! informational; missing binaries do not break the wider bench suite.
//!
//! # How to run
//!
//! One `oc-rsync` binary and an upstream reference are required:
//!
//! ```sh
//! # oc-rsync: INC_RECURSE is unconditional since ISI.h; the bench toggles
//! # it per-cell via the runtime `--no-inc-recursive` flag.
//! cargo build --release --bin oc-rsync
//!
//! # Upstream reference: rsync 3.4.1 (or any protocol >= 30 build)
//! bash tools/ci/run_interop.sh
//!
//! # Run the bench
//! OC_RSYNC_BIN=target/release/oc-rsync \
//! OC_RSYNC_BIN_UPSTREAM=target/interop/upstream-install/3.4.1/bin/rsync \
//! cargo bench -p transfer --bench isi_g_sender_inc_recurse_start_time
//! ```
//!
//! # Design references
//!
//! - ISI.a (`docs/design/isi-a-sender-inc-recurse-call-graph.md`) -
//!   sender-side call graph and the single-flip blocker that ISI.b
//!   lifted.
//! - ISI.b (`#4802`) - introduced the now-retired `sender-inc-recurse`
//!   cargo feature; ISI.h made INC_RECURSE unconditional and ISI.i.2
//!   removed the flag.
//! - ISI.c (`#4842`) - single-segment push interop.
//! - ISI.d (`#4846`) - multi-segment push interop.
//! - ISI.e (`#4859`) - wire-byte parity against upstream sender.
//! - Upstream `flist.c:46` `MIN_FILECNT_LOOKAHEAD = 1000` - segment
//!   scheduling throttle.
//! - Upstream `compat.c:720 set_allow_inc_recurse()` - the negotiation
//!   that the `'i'` capability bit triggers.

#![deny(unsafe_code)]

use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{ChildStdout, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use tempfile::TempDir;

/// Default file count for the fixture; override with `OC_RSYNC_BENCH_SCALE`.
///
/// 100 000 is the regime where the upstream INC_RECURSE design doc cites the
/// largest start-time wins; it is also large enough that the baseline (no
/// INC_RECURSE) spends a meaningful fraction of wall-clock building the
/// full flist before emitting any data.
const DEFAULT_FILE_COUNT: usize = 100_000;

/// Default cumulative-bytes threshold that defines "first byte of file data".
///
/// 1 MiB is well past the protocol greeting + capability negotiation
/// (~hundreds of bytes) and lands in the file-list serialization region for
/// both INC_RECURSE-on and INC_RECURSE-off. With INC_RECURSE off the sender
/// has to finish the entire flist before the first MSG_DATA frame escapes;
/// with INC_RECURSE on the first ~1 MiB streams out as soon as the initial
/// segment is built.
const DEFAULT_FIRST_DATA_BYTES: u64 = 1_048_576;

/// Files per directory in the fixture; kept constant when `OC_RSYNC_BENCH_SCALE`
/// scales the total file count so the per-directory shape stays comparable.
const FILES_PER_DIR: usize = 1_000;

/// Per-file payload size in bytes. Tiny on purpose: the bench measures
/// flist-construction latency, not codec throughput.
const FILE_PAYLOAD_BYTES: usize = 16;

/// Upstream rsync 3.4.1 protocol-32 capability string with `'i'` set.
///
/// Identical to the value used by ISI.c/d/e so the pipe push speaks the
/// same dialect across the test and bench harnesses.
const UPSTREAM_FLAGS_341: &str = "-vlogDtprze.iLsfxCIvu";

/// The two INC_RECURSE modes the bench compares, plus the upstream
/// reference. Since ISI.h, INC_RECURSE is unconditional on the sender side;
/// `Baseline` and `IncRecurse` now share the same `oc-rsync` binary and
/// differ only by the runtime `--no-inc-recursive` flag.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Sender {
    /// `oc-rsync` invoked with `--no-inc-recursive`. Capability string
    /// strips `'i'`; the upstream peer never enables INC_RECURSE.
    Baseline,
    /// `oc-rsync` invoked with its default flags. Capability string emits
    /// `'i'`; the peer enables INC_RECURSE on its compat flags.
    IncRecurse,
    /// Upstream `rsync`. Defaults to INC_RECURSE for protocol >= 30.
    Upstream,
}

impl Sender {
    fn label(self) -> &'static str {
        match self {
            Sender::Baseline => "baseline_no_inc_recurse",
            Sender::IncRecurse => "oc_rsync_sender_inc_recurse",
            Sender::Upstream => "upstream_rsync_3_4_1",
        }
    }

    /// Extra CLI args spliced into the sender invocation immediately after
    /// `--sender`. Only `Baseline` needs one; `IncRecurse` runs with the
    /// (now unconditional) default and `Upstream` is a separate binary.
    fn extra_args(self) -> &'static [&'static str] {
        match self {
            Sender::Baseline => &["--no-inc-recursive"],
            Sender::IncRecurse | Sender::Upstream => &[],
        }
    }

    fn env_var(self) -> &'static str {
        match self {
            Sender::Baseline | Sender::IncRecurse => "OC_RSYNC_BIN",
            Sender::Upstream => "OC_RSYNC_BIN_UPSTREAM",
        }
    }

    fn default_path(self) -> &'static str {
        match self {
            Sender::Baseline | Sender::IncRecurse => "target/release/oc-rsync",
            Sender::Upstream => "target/interop/upstream-install/3.4.1/bin/rsync",
        }
    }

    /// Resolves the binary path from the env var, falling back to the
    /// documented default. Returns `None` when neither resolves to a regular
    /// file; callers print a skip message and continue with the next cell.
    fn resolve_path(self) -> Option<PathBuf> {
        let candidate = std::env::var_os(self.env_var())
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(self.default_path()));
        if candidate.is_file() {
            Some(candidate)
        } else {
            None
        }
    }
}

/// Resolves the configured file count from `OC_RSYNC_BENCH_SCALE`, falling
/// back to `DEFAULT_FILE_COUNT`. Values <= 0 fall back to the default.
fn configured_file_count() -> usize {
    std::env::var("OC_RSYNC_BENCH_SCALE")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(DEFAULT_FILE_COUNT)
}

/// Resolves the configured first-data-bytes threshold from
/// `OC_RSYNC_BENCH_FIRST_DATA_BYTES`, falling back to `DEFAULT_FIRST_DATA_BYTES`.
fn configured_first_data_bytes() -> u64 {
    std::env::var("OC_RSYNC_BENCH_FIRST_DATA_BYTES")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(DEFAULT_FIRST_DATA_BYTES)
}

/// Returns true if `OC_RSYNC_BENCH_SKIP=1` is set.
fn skip_requested() -> bool {
    matches!(
        std::env::var("OC_RSYNC_BENCH_SKIP").ok().as_deref(),
        Some("1") | Some("true") | Some("TRUE")
    )
}

/// Build the source tree: `file_count` files spread across
/// `file_count / FILES_PER_DIR` directories with deterministic content so
/// every bench iteration sees the same source bytes.
///
/// Directory layout: `dir_NNNN/file_MMMMMM.bin`. Both indexes are
/// zero-padded so byte-for-byte ordering matches the lexicographic order
/// the receiver walks.
fn build_fixture(root: &Path, file_count: usize) -> io::Result<()> {
    fs::create_dir_all(root)?;
    let dir_count = file_count.div_ceil(FILES_PER_DIR);
    let mut payload = [0u8; FILE_PAYLOAD_BYTES];
    let mut written = 0usize;
    for d in 0..dir_count {
        let dir = root.join(format!("dir_{d:04}"));
        fs::create_dir_all(&dir)?;
        let in_this_dir = FILES_PER_DIR.min(file_count - written);
        for f in 0..in_this_dir {
            // Mix dir and file index into the payload so two same-offset
            // bytes in different files differ, defeating any same-content
            // dedup that an over-eager sender might apply.
            for (byte_idx, slot) in payload.iter_mut().enumerate() {
                *slot = ((d as u32)
                    .wrapping_mul(1009)
                    .wrapping_add((f as u32).wrapping_mul(31))
                    .wrapping_add(byte_idx as u32)) as u8;
            }
            let path = dir.join(format!("file_{f:06}.bin"));
            let mut file = File::create(&path)?;
            file.write_all(&payload)?;
        }
        written += in_this_dir;
        if written >= file_count {
            break;
        }
    }
    Ok(())
}

/// Pump bytes from `reader` to `writer` until EOF, flushing as we go.
/// Mirrors `copy_until_eof` from `inc_recurse_single_segment_push_isi_c.rs`.
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

/// Reader wrapper that records the wall-clock instant at which the
/// cumulative byte count crosses `threshold`. Once crossed, the recorded
/// instant is published into `first_data_at` and subsequent reads pay no
/// observation overhead beyond a single `Mutex` peek that finds the slot
/// already populated.
struct TimingReader {
    inner: ChildStdout,
    bytes_seen: u64,
    threshold: u64,
    first_data_at: Arc<Mutex<Option<Instant>>>,
}

impl Read for TimingReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        if n > 0 {
            self.bytes_seen += n as u64;
            if self.bytes_seen >= self.threshold {
                let mut slot = self.first_data_at.lock().unwrap();
                if slot.is_none() {
                    *slot = Some(Instant::now());
                }
            }
        }
        Ok(n)
    }
}

/// Result of a single timed push: time-to-first-data-bytes and total
/// transfer time. Both are measured from the moment the sender child is
/// spawned. `first_data` is `None` when the transfer never reached the
/// configured byte threshold - which indicates the bench config is
/// mis-tuned (threshold higher than the entire transfer payload).
struct PushTiming {
    first_data: Option<Duration>,
    total: Duration,
}

/// Drive one push: spawn the configured sender against an upstream
/// `--server` receiver, copy bytes between them, and record both timings.
fn run_one_push(
    sender_bin: &Path,
    extra_sender_args: &[&str],
    receiver_bin: &Path,
    src: &Path,
    dst: &Path,
    threshold: u64,
) -> io::Result<PushTiming> {
    let spawned_at = Instant::now();

    let mut server = Command::new(sender_bin)
        .arg("--server")
        .arg("--sender")
        .args(extra_sender_args)
        .arg(UPSTREAM_FLAGS_341)
        .arg(".")
        .arg(src.to_string_lossy().as_ref())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    let mut client = Command::new(receiver_bin)
        .arg("--server")
        .arg(UPSTREAM_FLAGS_341)
        .arg(".")
        .arg(dst.to_string_lossy().as_ref())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    let server_stdout = server.stdout.take().expect("server stdout");
    let server_stdin = server.stdin.take().expect("server stdin");
    let client_stdout = client.stdout.take().expect("client stdout");
    let client_stdin = client.stdin.take().expect("client stdin");

    let first_data_at: Arc<Mutex<Option<Instant>>> = Arc::new(Mutex::new(None));

    let timing_reader = TimingReader {
        inner: server_stdout,
        bytes_seen: 0,
        threshold,
        first_data_at: Arc::clone(&first_data_at),
    };

    let s2c = thread::spawn(move || -> io::Result<()> {
        let mut reader = BufReader::new(timing_reader);
        let mut writer = BufWriter::new(client_stdin);
        copy_until_eof(&mut reader, &mut writer)
    });

    let c2s = thread::spawn(move || -> io::Result<()> {
        let mut reader = BufReader::new(client_stdout);
        let mut writer = BufWriter::new(server_stdin);
        copy_until_eof(&mut reader, &mut writer)
    });

    let server_stderr = server.stderr.take();
    let client_stderr = client.stderr.take();

    let server_status = server.wait()?;
    let client_status = client.wait()?;
    let _ = s2c.join();
    let _ = c2s.join();
    let total = spawned_at.elapsed();

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
            "pipe push failed: sender={:?} client={:?}\nsender stderr:\n{}\nclient stderr:\n{}",
            server_status.code(),
            client_status.code(),
            String::from_utf8_lossy(&server_err),
            String::from_utf8_lossy(&client_err),
        )));
    }

    let first_data = first_data_at
        .lock()
        .unwrap()
        .map(|inst| inst.duration_since(spawned_at));

    drop(server_stderr);
    drop(client_stderr);
    drop(client);
    drop(server);

    Ok(PushTiming { first_data, total })
}

/// Bench one sender variant. Skips with a printed message when the binary
/// is missing or the upstream receiver is unavailable.
fn bench_sender(
    c: &mut Criterion,
    sender: Sender,
    src_root: &Path,
    file_count: usize,
    first_data_threshold: u64,
) {
    let Some(sender_bin) = sender.resolve_path() else {
        eprintln!(
            "isi_g skip: {} binary missing (set {}={})",
            sender.label(),
            sender.env_var(),
            sender.default_path()
        );
        return;
    };

    let Some(receiver_bin) = Sender::Upstream.resolve_path() else {
        eprintln!(
            "isi_g skip: upstream receiver binary missing (set {}={})",
            Sender::Upstream.env_var(),
            Sender::Upstream.default_path()
        );
        return;
    };

    let extra_args = sender.extra_args();
    let group_name = format!("isi_g_sender_inc_recurse_start_time/{}", sender.label());
    let mut group = c.benchmark_group(group_name);
    // Each iteration spawns two processes and pumps the full 100 000-file
    // flist through pipes. Cap sample count to keep total wall-clock under
    // criterion's default 5-minute soft limit.
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(60));

    let bench_id = BenchmarkId::new(
        format!("files_{file_count}"),
        format!("threshold_{first_data_threshold}"),
    );

    group.bench_function(bench_id, |b| {
        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                let dest = TempDir::new().expect("dest tempdir");
                let timing = run_one_push(
                    &sender_bin,
                    extra_args,
                    &receiver_bin,
                    src_root,
                    dest.path(),
                    first_data_threshold,
                )
                .expect("pipe push must succeed");
                // Criterion drives sample wall-clock from `total`; the
                // first-data observation is folded into the sample mean
                // by adding it as the timing. The total transfer time is
                // emitted as a sibling bench below so both numbers are
                // visible without doubling the per-iteration cost.
                total += timing.first_data.unwrap_or(timing.total);
            }
            total
        });
    });

    group.finish();

    // Sibling group: total transfer time. Same iteration shape; the second
    // pass keeps the bench output readable (one number per metric per
    // sender) at the cost of running each push twice. With ten samples per
    // group the extra cost stays inside the 60-second measurement window.
    let total_group_name = format!(
        "isi_g_sender_inc_recurse_start_time/{}__total_transfer",
        sender.label()
    );
    let mut total_group = c.benchmark_group(total_group_name);
    total_group.sample_size(10);
    total_group.measurement_time(Duration::from_secs(60));
    total_group.bench_function(format!("files_{file_count}"), |b| {
        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                let dest = TempDir::new().expect("dest tempdir");
                let timing = run_one_push(
                    &sender_bin,
                    extra_args,
                    &receiver_bin,
                    src_root,
                    dest.path(),
                    first_data_threshold,
                )
                .expect("pipe push must succeed");
                total += timing.total;
            }
            total
        });
    });
    total_group.finish();
}

/// Top-level entry. Builds the shared source fixture once, then runs all
/// three sender variants against it.
fn bench_isi_g(c: &mut Criterion) {
    if skip_requested() {
        eprintln!("isi_g skip: OC_RSYNC_BENCH_SKIP set");
        return;
    }

    let file_count = configured_file_count();
    let first_data_threshold = configured_first_data_bytes();

    let fixture_root = match TempDir::new() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("isi_g skip: could not create tempdir: {e}");
            return;
        }
    };
    let src = fixture_root.path().join("src");
    if let Err(e) = build_fixture(&src, file_count) {
        eprintln!("isi_g skip: could not build fixture ({file_count} files): {e}");
        return;
    }

    for sender in [Sender::Baseline, Sender::IncRecurse, Sender::Upstream] {
        bench_sender(c, sender, &src, file_count, first_data_threshold);
    }
}

criterion_group!(
    name = isi_g_benches;
    config = Criterion::default()
        .warm_up_time(Duration::from_secs(2));
    targets = bench_isi_g
);

criterion_main!(isi_g_benches);
