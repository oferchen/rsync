use super::config::{BenchmarkOptions, DEFAULT_DAEMON_PORT, DEFAULT_RUNS, profile_params};
use super::loopback::{generate_loopback_data, port_available, walkdir_size};
use super::runner::{BenchmarkResult, RunSample, parse_stat_value, parse_stats_output};
use crate::cli::DataProfile;
use std::fs;
use std::time::Duration;

#[test]
fn benchmark_result_calculates_stats() {
    let samples = vec![
        RunSample {
            elapsed: Duration::from_millis(100),
            bytes_sent: 100,
            bytes_received: 1000,
            total_size: 2000,
        },
        RunSample {
            elapsed: Duration::from_millis(110),
            bytes_sent: 100,
            bytes_received: 1000,
            total_size: 2000,
        },
        RunSample {
            elapsed: Duration::from_millis(90),
            bytes_sent: 100,
            bytes_received: 1000,
            total_size: 2000,
        },
        RunSample {
            elapsed: Duration::from_millis(105),
            bytes_sent: 100,
            bytes_received: 1000,
            total_size: 2000,
        },
        RunSample {
            elapsed: Duration::from_millis(95),
            bytes_sent: 100,
            bytes_received: 1000,
            total_size: 2000,
        },
    ];
    let result = BenchmarkResult::new("test".to_string(), samples);

    assert_eq!(result.min, Duration::from_millis(90));
    assert_eq!(result.max, Duration::from_millis(110));
    // Mean should be 100ms
    assert!((result.mean.as_millis() as i64 - 100).abs() <= 1);
    assert!(result.mean_throughput_mbps > 0.0);
}

#[test]
fn parse_stats_output_extracts_values() {
    let stats = "\
Number of files: 42 (reg: 40, dir: 2)
Total file size: 1,234,567 bytes
Total bytes sent: 456
Total bytes received: 1,234,567
";
    let sample = parse_stats_output(stats, Duration::from_secs(1));
    assert_eq!(sample.bytes_sent, 456);
    assert_eq!(sample.bytes_received, 1_234_567);
    assert_eq!(sample.total_size, 1_234_567);
}

#[test]
fn parse_stat_value_handles_commas() {
    assert_eq!(parse_stat_value(" 1,234,567 bytes"), 1_234_567);
    assert_eq!(parse_stat_value(" 42"), 42);
    assert_eq!(parse_stat_value(" 0"), 0);
}

#[test]
fn default_options_are_sensible() {
    let options = BenchmarkOptions::default();
    assert_eq!(options.port, DEFAULT_DAEMON_PORT);
    assert_eq!(options.runs, DEFAULT_RUNS);
    assert!(!options.skip_build);
    assert_eq!(options.data_profile, DataProfile::Medium);
}

#[test]
fn profile_params_returns_expected_sizes() {
    assert_eq!(profile_params(DataProfile::Small), (1_000, 10));
    assert_eq!(profile_params(DataProfile::Medium), (10_000, 100));
    assert_eq!(profile_params(DataProfile::Large), (50_000, 500));
}

#[test]
fn generate_loopback_data_creates_files() {
    let tmp = std::env::temp_dir().join("xtask-bench-test-gen");
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&tmp).unwrap();

    // Use small profile but verify structure
    let data_dir = generate_loopback_data(&tmp, DataProfile::Small).unwrap();
    assert!(data_dir.exists());
    assert!(data_dir.join(".profile").exists());

    let marker = fs::read_to_string(data_dir.join(".profile")).unwrap();
    assert_eq!(marker.trim(), "Small");

    // Verify some files were created
    let total = walkdir_size(&data_dir);
    assert!(total > 0, "expected non-zero total size");

    // Verify idempotency (second call reuses existing data)
    let data_dir2 = generate_loopback_data(&tmp, DataProfile::Small).unwrap();
    assert_eq!(data_dir, data_dir2);

    let _ = fs::remove_dir_all(&tmp);
}

#[test]
fn walkdir_size_counts_files() {
    let tmp = std::env::temp_dir().join("xtask-bench-test-walkdir");
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(tmp.join("sub")).unwrap();
    fs::write(tmp.join("a.txt"), "hello").unwrap();
    fs::write(tmp.join("sub/b.txt"), "world!").unwrap();

    let total = walkdir_size(&tmp);
    assert_eq!(total, 11); // "hello" (5) + "world!" (6)

    let _ = fs::remove_dir_all(&tmp);
}

#[test]
fn port_available_finds_open_port() {
    // Port 0 is never bindable in the normal sense, but high ports should be available
    // Just verify the function doesn't panic
    let _ = port_available(59999);
}
