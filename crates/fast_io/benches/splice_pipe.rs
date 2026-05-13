//! Benchmark for splice/vmsplice zero-copy transfer paths.
//!
//! Compares three data movement strategies for writing file/buffer data
//! through a pipe (simulating the SSH stdio path):
//!
//! - `read_write_baseline` - standard `read(2)` + `write_all(2)` with a
//!   64 KiB buffer. This mirrors the existing sender code path.
//! - `splice_transfer` - `splice(file_fd, pipe) + splice(pipe, dest_fd)`
//!   zero-copy path via [`fast_io::splice::SplicePipe`].
//! - `vmsplice_transfer` - `vmsplice(buf, pipe) + splice(pipe, dest_fd)`
//!   for transferring an in-memory buffer to a file without userspace copy.
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
    use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd};
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

    /// Creates a destination temp file and returns (file, raw_fd).
    fn make_dest_file() -> (NamedTempFile, i32) {
        let file = NamedTempFile::new().expect("create dest temp file");
        let fd = file.as_file().as_raw_fd();
        (file, fd)
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

    // splice(2) bench: file -> splice -> pipe -> splice -> dest file.
    if fast_io::splice::is_splice_available() {
        for &(label, size) in SIZES {
            let payload = create_payload(size);
            group.throughput(Throughput::Bytes(size as u64));
            group.bench_with_input(BenchmarkId::new("splice_transfer", label), &(), |b, _| {
                b.iter_with_setup(
                    || {
                        let src = File::open(payload.path()).expect("open payload");
                        let (dest, dest_fd) = make_dest_file();
                        (src, dest, dest_fd)
                    },
                    |(src, _dest, dest_fd)| {
                        let src_fd = src.as_raw_fd();
                        let n = fast_io::splice::try_splice_to_file(src_fd, dest_fd, size)
                            .expect("splice_to_file");
                        black_box(n);
                    },
                );
            });
        }

        // vmsplice(2) bench: in-memory buffer -> vmsplice -> pipe -> splice -> dest file.
        for &(label, size) in SIZES {
            let buf: Vec<u8> = (0..size).map(|i| (i % 251) as u8).collect();
            group.throughput(Throughput::Bytes(size as u64));
            group.bench_with_input(BenchmarkId::new("vmsplice_transfer", label), &(), |b, _| {
                b.iter_with_setup(
                    || {
                        let (dest, dest_fd) = make_dest_file();
                        (dest, dest_fd)
                    },
                    |(_dest, dest_fd)| {
                        let n = fast_io::splice::try_vmsplice_to_file(&buf, dest_fd)
                            .expect("vmsplice_to_file");
                        black_box(n);
                    },
                );
            });
        }
    } else {
        eprintln!("splice(2) not available on this kernel - skipping splice/vmsplice benches");
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
