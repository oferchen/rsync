//! Benchmarks for CLI startup overhead - argument parsing and help generation.
//!
//! Measures the cost of parsing common flag combinations and rendering help text,
//! which dominate the fixed overhead of every oc-rsync invocation.
//!
//! Run with: `cargo bench -p cli -- startup`

use criterion::{Criterion, criterion_group, criterion_main};
use std::hint::black_box;

use cli::test_utils::parse_args;

/// Benchmark parsing `-avz --delete src/ dst/` - the most common invocation pattern.
fn bench_parse_avz_delete(c: &mut Criterion) {
    c.bench_function("parse_avz_delete", |b| {
        b.iter(|| {
            let result = parse_args(black_box(["oc-rsync", "-avz", "--delete", "src/", "dst/"]));
            black_box(result).ok();
        });
    });
}

/// Benchmark parsing `-r --exclude '*.tmp' src/ dst/` - recursive with filter.
fn bench_parse_recursive_exclude(c: &mut Criterion) {
    c.bench_function("parse_recursive_exclude", |b| {
        b.iter(|| {
            let result = parse_args(black_box([
                "oc-rsync",
                "-r",
                "--exclude",
                "*.tmp",
                "src/",
                "dst/",
            ]));
            black_box(result).ok();
        });
    });
}

/// Benchmark parsing a complex flag set typical of backup scripts.
fn bench_parse_backup_flags(c: &mut Criterion) {
    c.bench_function("parse_backup_flags", |b| {
        b.iter(|| {
            let result = parse_args(black_box([
                "oc-rsync",
                "-avz",
                "--delete",
                "--delete-excluded",
                "--exclude",
                ".git",
                "--exclude",
                "*.tmp",
                "--exclude",
                "node_modules",
                "--bwlimit=10000",
                "--partial",
                "--progress",
                "--stats",
                "src/",
                "dst/",
            ]));
            black_box(result).ok();
        });
    });
}

/// Benchmark parsing `--dry-run -r src/ dst/` - minimal flags for a dry run.
fn bench_parse_dry_run(c: &mut Criterion) {
    c.bench_function("parse_dry_run", |b| {
        b.iter(|| {
            let result = parse_args(black_box(["oc-rsync", "--dry-run", "-r", "src/", "dst/"]));
            black_box(result).ok();
        });
    });
}

/// Benchmark parsing `--version` - the simplest invocation.
fn bench_parse_version(c: &mut Criterion) {
    c.bench_function("parse_version", |b| {
        b.iter(|| {
            let result = parse_args(black_box(["oc-rsync", "--version"]));
            black_box(result).ok();
        });
    });
}

/// Benchmark parsing `--help` - triggers help text recognition.
fn bench_parse_help(c: &mut Criterion) {
    c.bench_function("parse_help", |b| {
        b.iter(|| {
            let result = parse_args(black_box(["oc-rsync", "--help"]));
            black_box(result).ok();
        });
    });
}

/// Benchmark parsing with many short options combined.
fn bench_parse_combined_short_options(c: &mut Criterion) {
    c.bench_function("parse_combined_short_opts", |b| {
        b.iter(|| {
            let result = parse_args(black_box([
                "oc-rsync",
                "-rlptgoD",
                "--compress",
                "--sparse",
                "src/",
                "dst/",
            ]));
            black_box(result).ok();
        });
    });
}

/// Benchmark parsing server-mode args (internal protocol use).
fn bench_parse_server_sender(c: &mut Criterion) {
    c.bench_function("parse_server_sender", |b| {
        b.iter(|| {
            let result = parse_args(black_box([
                "oc-rsync",
                "--server",
                "--sender",
                "-vlogDtpre.iLsfxCIvu",
                ".",
                "src/",
            ]));
            black_box(result).ok();
        });
    });
}

criterion_group!(
    startup_benches,
    bench_parse_avz_delete,
    bench_parse_recursive_exclude,
    bench_parse_backup_flags,
    bench_parse_dry_run,
    bench_parse_version,
    bench_parse_help,
    bench_parse_combined_short_options,
    bench_parse_server_sender,
);
criterion_main!(startup_benches);
