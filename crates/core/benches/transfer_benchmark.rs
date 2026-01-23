//! Real rsync:// transfer benchmarks.
//!
//! These benchmarks test actual daemon-to-client transfers to measure
//! realistic performance characteristics including:
//! - Protocol negotiation overhead
//! - File list building and transmission
//! - Delta transfer efficiency
//! - Small file handling
//! - Large file throughput
//!
//! Run with: `cargo bench -p core --bench transfer_benchmark`
//!
//! Note: Requires upstream rsync binaries in target/interop/upstream-install/

use std::fs::{self, File};
use std::io::Write;
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU16, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use tempfile::TempDir;

// Upstream binary paths
const UPSTREAM_341: &str = "target/interop/upstream-install/3.4.1/bin/rsync";
const OC_RSYNC: &str = "target/release/oc-rsync";

// Port allocation for benchmarks
static NEXT_PORT: AtomicU16 = AtomicU16::new(14000);

fn get_port() -> u16 {
    NEXT_PORT.fetch_add(1, Ordering::SeqCst)
}

/// Managed upstream rsync daemon for benchmarking.
struct BenchDaemon {
    _workdir: TempDir,
    module_path: PathBuf,
    port: u16,
    process: Child,
}

impl BenchDaemon {
    fn start() -> Option<Self> {
        if !Path::new(UPSTREAM_341).exists() {
            return None;
        }

        let workdir = TempDir::new().ok()?;
        let port = get_port();
        let config_path = workdir.path().join("rsyncd.conf");
        let pid_path = workdir.path().join("rsyncd.pid");
        let module_path = workdir.path().join("module");

        fs::create_dir_all(&module_path).ok()?;

        let config = format!(
            "pid file = {}\nport = {}\nuse chroot = false\nnumeric ids = yes\n\n\
             [bench]\n    path = {}\n    read only = false\n",
            pid_path.display(),
            port,
            module_path.display()
        );
        fs::write(&config_path, config).ok()?;

        let process = Command::new(UPSTREAM_341)
            .arg("--daemon")
            .arg("--config")
            .arg(&config_path)
            .arg("--no-detach")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;

        // Wait for daemon to be ready
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if TcpStream::connect(format!("127.0.0.1:{port}")).is_ok() {
                return Some(Self {
                    _workdir: workdir,
                    module_path,
                    port,
                    process,
                });
            }
            thread::sleep(Duration::from_millis(50));
        }
        None
    }

    fn module_url(&self) -> String {
        format!("rsync://127.0.0.1:{}/bench/", self.port)
    }

    fn populate_small_files(&self, count: usize, size: usize) {
        let data = vec![b'x'; size];
        for i in 0..count {
            let path = self.module_path.join(format!("small_{i}.dat"));
            fs::write(&path, &data).expect("write small file");
        }
    }

    fn populate_large_file(&self, name: &str, size: usize) {
        let path = self.module_path.join(name);
        let mut file = File::create(&path).expect("create large file");
        let chunk = vec![b'L'; 64 * 1024];
        let mut remaining = size;
        while remaining > 0 {
            let write_size = remaining.min(chunk.len());
            file.write_all(&chunk[..write_size]).expect("write chunk");
            remaining -= write_size;
        }
    }

    fn populate_mixed_tree(&self, dirs: usize, files_per_dir: usize) {
        for d in 0..dirs {
            let dir = self.module_path.join(format!("dir_{d}"));
            fs::create_dir_all(&dir).expect("create dir");
            for f in 0..files_per_dir {
                let path = dir.join(format!("file_{f}.txt"));
                fs::write(&path, format!("content for dir {d} file {f}")).expect("write");
            }
        }
    }

    fn clear(&self) {
        for entry in fs::read_dir(&self.module_path).expect("read module") {
            let entry = entry.expect("entry");
            let path = entry.path();
            if path.is_dir() {
                fs::remove_dir_all(&path).expect("remove dir");
            } else {
                fs::remove_file(&path).expect("remove file");
            }
        }
    }
}

impl Drop for BenchDaemon {
    fn drop(&mut self) {
        let _ = self.process.kill();
        let _ = self.process.wait();
    }
}

/// Run upstream rsync and measure time.
fn run_upstream(source: &str, dest: &Path, extra_args: &[&str]) -> Duration {
    let start = Instant::now();
    let status = Command::new(UPSTREAM_341)
        .args(extra_args)
        .arg("-a")
        .arg(source)
        .arg(dest)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("run upstream");
    assert!(status.success(), "upstream rsync failed");
    start.elapsed()
}

/// Run oc-rsync and measure time.
fn run_oc_rsync(source: &str, dest: &Path, extra_args: &[&str]) -> Duration {
    if !Path::new(OC_RSYNC).exists() {
        panic!("oc-rsync not found at {OC_RSYNC}. Run: cargo build --release");
    }
    let start = Instant::now();
    let status = Command::new(OC_RSYNC)
        .args(extra_args)
        .arg("-a")
        .arg(source)
        .arg(dest)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("run oc-rsync");
    assert!(status.success(), "oc-rsync failed");
    start.elapsed()
}

/// Benchmark: Pull many small files from daemon.
fn bench_small_files_pull(c: &mut Criterion) {
    let Some(daemon) = BenchDaemon::start() else {
        eprintln!("Skipping: upstream rsync not available");
        return;
    };

    let mut group = c.benchmark_group("daemon_pull_small_files");

    for count in [100, 500, 1000] {
        daemon.clear();
        daemon.populate_small_files(count, 1024); // 1KB files
        let total_bytes = count * 1024;

        group.throughput(Throughput::Bytes(total_bytes as u64));

        let dest_up = TempDir::new().unwrap();
        group.bench_with_input(
            BenchmarkId::new("upstream", count),
            &count,
            |b, _| {
                b.iter_custom(|iters| {
                    let mut total = Duration::ZERO;
                    for _ in 0..iters {
                        let _ = fs::remove_dir_all(dest_up.path());
                        fs::create_dir_all(dest_up.path()).unwrap();
                        total += run_upstream(&daemon.module_url(), dest_up.path(), &[]);
                    }
                    total
                });
            },
        );

        let dest_oc = TempDir::new().unwrap();
        group.bench_with_input(BenchmarkId::new("oc-rsync", count), &count, |b, _| {
            b.iter_custom(|iters| {
                let mut total = Duration::ZERO;
                for _ in 0..iters {
                    let _ = fs::remove_dir_all(dest_oc.path());
                    fs::create_dir_all(dest_oc.path()).unwrap();
                    total += run_oc_rsync(&daemon.module_url(), dest_oc.path(), &[]);
                }
                total
            });
        });
    }

    group.finish();
}

/// Benchmark: Pull large file from daemon.
fn bench_large_file_pull(c: &mut Criterion) {
    let Some(daemon) = BenchDaemon::start() else {
        eprintln!("Skipping: upstream rsync not available");
        return;
    };

    let mut group = c.benchmark_group("daemon_pull_large_file");

    for size_mb in [10, 50, 100] {
        daemon.clear();
        let size = size_mb * 1024 * 1024;
        daemon.populate_large_file("large.dat", size);

        group.throughput(Throughput::Bytes(size as u64));

        let dest_up = TempDir::new().unwrap();
        group.bench_with_input(
            BenchmarkId::new("upstream", format!("{size_mb}MB")),
            &size,
            |b, _| {
                b.iter_custom(|iters| {
                    let mut total = Duration::ZERO;
                    for _ in 0..iters {
                        let _ = fs::remove_dir_all(dest_up.path());
                        fs::create_dir_all(dest_up.path()).unwrap();
                        total += run_upstream(&daemon.module_url(), dest_up.path(), &[]);
                    }
                    total
                });
            },
        );

        let dest_oc = TempDir::new().unwrap();
        group.bench_with_input(
            BenchmarkId::new("oc-rsync", format!("{size_mb}MB")),
            &size,
            |b, _| {
                b.iter_custom(|iters| {
                    let mut total = Duration::ZERO;
                    for _ in 0..iters {
                        let _ = fs::remove_dir_all(dest_oc.path());
                        fs::create_dir_all(dest_oc.path()).unwrap();
                        total += run_oc_rsync(&daemon.module_url(), dest_oc.path(), &[]);
                    }
                    total
                });
            },
        );
    }

    group.finish();
}

/// Benchmark: Incremental sync (no changes).
fn bench_incremental_no_change(c: &mut Criterion) {
    let Some(daemon) = BenchDaemon::start() else {
        eprintln!("Skipping: upstream rsync not available");
        return;
    };

    daemon.clear();
    daemon.populate_mixed_tree(10, 50); // 500 files

    let mut group = c.benchmark_group("daemon_incremental_no_change");
    group.throughput(Throughput::Elements(500));

    // Pre-sync for both
    let dest_up = TempDir::new().unwrap();
    run_upstream(&daemon.module_url(), dest_up.path(), &[]);

    let dest_oc = TempDir::new().unwrap();
    run_oc_rsync(&daemon.module_url(), dest_oc.path(), &[]);

    group.bench_function("upstream", |b| {
        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                total += run_upstream(&daemon.module_url(), dest_up.path(), &[]);
            }
            total
        });
    });

    group.bench_function("oc-rsync", |b| {
        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                total += run_oc_rsync(&daemon.module_url(), dest_oc.path(), &[]);
            }
            total
        });
    });

    group.finish();
}

/// Benchmark: Directory tree traversal.
fn bench_deep_tree(c: &mut Criterion) {
    let Some(daemon) = BenchDaemon::start() else {
        eprintln!("Skipping: upstream rsync not available");
        return;
    };

    let mut group = c.benchmark_group("daemon_deep_tree");

    for depth in [5, 10] {
        daemon.clear();

        // Create deep directory tree
        let mut path = daemon.module_path.clone();
        for i in 0..depth {
            path = path.join(format!("level_{i}"));
            fs::create_dir_all(&path).unwrap();
            fs::write(path.join("file.txt"), format!("depth {i}")).unwrap();
        }

        group.throughput(Throughput::Elements(depth as u64));

        let dest_up = TempDir::new().unwrap();
        group.bench_with_input(
            BenchmarkId::new("upstream", depth),
            &depth,
            |b, _| {
                b.iter_custom(|iters| {
                    let mut total = Duration::ZERO;
                    for _ in 0..iters {
                        let _ = fs::remove_dir_all(dest_up.path());
                        fs::create_dir_all(dest_up.path()).unwrap();
                        total += run_upstream(&daemon.module_url(), dest_up.path(), &[]);
                    }
                    total
                });
            },
        );

        let dest_oc = TempDir::new().unwrap();
        group.bench_with_input(BenchmarkId::new("oc-rsync", depth), &depth, |b, _| {
            b.iter_custom(|iters| {
                let mut total = Duration::ZERO;
                for _ in 0..iters {
                    let _ = fs::remove_dir_all(dest_oc.path());
                    fs::create_dir_all(dest_oc.path()).unwrap();
                    total += run_oc_rsync(&daemon.module_url(), dest_oc.path(), &[]);
                }
                total
            });
        });
    }

    group.finish();
}

criterion_group!(
    transfer_benchmarks,
    bench_small_files_pull,
    bench_large_file_pull,
    bench_incremental_no_change,
    bench_deep_tree,
);

criterion_main!(transfer_benchmarks);
