//! Benchmark skeleton for the splice/vmsplice SSH-stdio investigation.
//!
//! Tracks oc-rsync task #1860. The audit doc lives at
//! `docs/audits/splice-ssh-stdio.md`. This bench provides a baseline so that
//! once the splice driver lands (`fast_io::splice_pipe`, see follow-up
//! #1861), a measurable delta can be reported against the read/write loop
//! that oc-rsync uses today on the SSH-stdio sender path.
//!
//! Two benches are defined inside the `splice_pipe` group:
//!
//! - `read_write_baseline` - a real file -> pipe transfer using
//!   `read(2)` + `write_all(2)` with a 64 KiB buffer. This mirrors the
//!   current sender code path (modulo the multiplex envelope) and produces
//!   the comparison baseline for future splice work.
//! - `splice_baseline` - a placeholder slot that today just records a
//!   constant. It exists so that the criterion group has a stable identity
//!   on Linux and so that follow-up patches can drop the splice driver into
//!   place without changing the report layout. The body is intentionally
//!   trivial; replace it once #1861 lands.
//!
//! Cross-platform: the meaningful code is gated to Linux. On other targets
//! the bench compiles to an empty criterion main so that
//! `cargo bench -p fast_io --bench splice_pipe` does not fail to build on
//! macOS or Windows. CI runs the meaningful benches on Linux only.
//!
//! Run with: `cargo bench -p fast_io --bench splice_pipe`

use criterion::{Criterion, criterion_group, criterion_main};

#[cfg(target_os = "linux")]
fn bench_splice_pipe(c: &mut Criterion) {
    use std::fs::File;
    use std::io::{Read, Write};
    use std::os::fd::{FromRawFd, IntoRawFd, OwnedFd};
    use std::thread;

    use criterion::{BenchmarkId, Throughput};
    use std::hint::black_box;
    use tempfile::NamedTempFile;

    /// Payload sizes exercised by the bench. 64 KiB matches the default
    /// Linux pipe buffer; 1 MiB and 16 MiB stress the loop and the multiplex
    /// max-payload boundary respectively.
    const SIZES: &[(&str, usize)] = &[
        ("64KB", 64 * 1024),
        ("1MB", 1024 * 1024),
        ("16MB", 16 * 1024 * 1024),
    ];

    /// Buffer size used by the read/write baseline. Mirrors the chunk size
    /// that oc-rsync's existing sender path uses for whole-file transfers.
    const BUFFER_SIZE: usize = 64 * 1024;

    fn create_payload(size: usize) -> NamedTempFile {
        let mut file = NamedTempFile::new().expect("create temp file");
        let chunk: Vec<u8> = (0..BUFFER_SIZE).map(|i| (i % 251) as u8).collect();
        let mut remaining = size;
        while remaining > 0 {
            let n = remaining.min(BUFFER_SIZE);
            file.write_all(&chunk[..n]).expect("write payload");
            remaining -= n;
        }
        file.flush().expect("flush payload");
        file
    }

    /// Creates a pipe pair via `pipe(2)`. The reader fd is wrapped in a
    /// drain thread to keep the pipe from filling up; the writer fd is
    /// returned to the caller as an `OwnedFd`.
    fn make_drained_pipe() -> (OwnedFd, thread::JoinHandle<()>) {
        let mut fds = [0i32; 2];
        // SAFETY: pipe(2) writes two fresh fds into the array. We immediately
        // wrap each fd in OwnedFd / File so ownership is well-defined.
        let rc = unsafe { libc::pipe(fds.as_mut_ptr()) };
        assert_eq!(rc, 0, "pipe(2) failed: {}", std::io::Error::last_os_error());
        // SAFETY: fds[0] is a fresh pipe read fd we own.
        let read_end = unsafe { File::from_raw_fd(fds[0]) };
        // SAFETY: fds[1] is a fresh pipe write fd we own.
        let write_end = unsafe { OwnedFd::from_raw_fd(fds[1]) };

        let drain = thread::spawn(move || {
            let mut reader = read_end;
            let mut sink = [0u8; BUFFER_SIZE];
            loop {
                match reader.read(&mut sink) {
                    Ok(0) => break,
                    Ok(_) => continue,
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => break,
                }
            }
        });

        (write_end, drain)
    }

    /// Drives the read/write baseline once. Reads `payload` from start to end
    /// into a 64 KiB buffer and writes it through `writer` (a pipe).
    fn run_read_write(payload: &NamedTempFile, writer: OwnedFd) {
        // SAFETY: `writer` is a fresh OwnedFd handed to us by the bench
        // setup; converting it to a File transfers ownership cleanly.
        let mut writer = unsafe { File::from_raw_fd(writer.into_raw_fd()) };
        let mut src = File::open(payload.path()).expect("open payload");
        let mut buffer = vec![0u8; BUFFER_SIZE];
        loop {
            match src.read(&mut buffer) {
                Ok(0) => break,
                Ok(n) => {
                    writer.write_all(&buffer[..n]).expect("write_all");
                    black_box(n);
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => panic!("read failed: {e}"),
            }
        }
    }

    let mut group = c.benchmark_group("splice_pipe");
    group.sample_size(10);

    for &(label, size) in SIZES {
        let payload = create_payload(size);
        group.throughput(Throughput::Bytes(size as u64));
        group.bench_with_input(
            BenchmarkId::new("read_write_baseline", label),
            &(),
            |b, _| {
                b.iter_with_setup(make_drained_pipe, |(writer_fd, drain)| {
                    run_read_write(&payload, writer_fd);
                    drain.join().expect("drain thread");
                });
            },
        );
    }

    eprintln!(
        "splice_pipe::splice_baseline is a placeholder; tracking #1861 \
         for the real fast_io::splice_pipe driver"
    );
    for &(label, size) in SIZES {
        group.throughput(Throughput::Bytes(size as u64));
        group.bench_with_input(
            BenchmarkId::new("splice_baseline", label),
            &size,
            |b, &size| {
                b.iter(|| black_box(size as u64));
            },
        );
    }

    group.finish();
}

#[cfg(not(target_os = "linux"))]
fn bench_splice_pipe(c: &mut Criterion) {
    use std::hint::black_box;

    // splice(2) is Linux-only. Define an empty group on other targets so the
    // criterion harness still emits a report and the bench binary compiles.
    let mut group = c.benchmark_group("splice_pipe");
    group.sample_size(10);
    group.bench_function("noop_non_linux", |b| {
        b.iter(|| black_box(0u64));
    });
    group.finish();
}

criterion_group!(splice_pipe, bench_splice_pipe);
criterion_main!(splice_pipe);
