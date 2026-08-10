//! Output formatting for benchmark results (text table and JSON).

use crate::error::TaskResult;
use std::time::Duration;

use super::runner::BenchmarkResult;

/// Result set for a single URL.
#[derive(Debug)]
pub(super) struct BenchmarkResultSet {
    pub(super) url: String,
    pub(super) url_name: String,
    pub(super) results: Vec<BenchmarkResult>,
}

/// Outputs results for multiple URLs as formatted tables.
pub(super) fn output_table_multi(result_sets: &[BenchmarkResultSet]) {
    println!("\n=== Benchmark Results ===\n");

    for result_set in result_sets {
        println!("--- {} ({}) ---\n", result_set.url_name, result_set.url);
        println!(
            "{:<12} {:>10} {:>10} {:>10} {:>10} {:>12}",
            "Version", "Mean", "Min", "Max", "Stddev", "Throughput"
        );
        println!("{}", "-".repeat(68));

        let baseline = result_set
            .results
            .first()
            .map(|r| r.mean)
            .unwrap_or(Duration::ZERO);

        for result in &result_set.results {
            let diff_pct = if baseline > Duration::ZERO {
                ((result.mean.as_secs_f64() - baseline.as_secs_f64()) / baseline.as_secs_f64())
                    * 100.0
            } else {
                0.0
            };

            let speedup_str = if diff_pct.abs() < 1.0 {
                String::new()
            } else if diff_pct > 0.0 {
                format!(" ({diff_pct:.1}% slower)")
            } else {
                let faster = -diff_pct;
                format!(" ({faster:.1}% faster)")
            };

            let throughput_str = if result.mean_throughput_mbps > 0.0 {
                format!("{:.2} MB/s", result.mean_throughput_mbps)
            } else {
                "-".to_string()
            };

            println!(
                "{:<12} {:>10.3}s {:>10.3}s {:>10.3}s {:>10.4}s {:>12}{}",
                result.version,
                result.mean.as_secs_f64(),
                result.min.as_secs_f64(),
                result.max.as_secs_f64(),
                result.stddev,
                throughput_str,
                speedup_str
            );
        }
        println!();
    }

    if result_sets.len() > 1 {
        print_summary_table(result_sets);
    }
}

/// Prints a summary table comparing all versions across all URLs.
fn print_summary_table(result_sets: &[BenchmarkResultSet]) {
    println!("=== Summary (Mean times in seconds) ===\n");

    let mut versions: Vec<String> = Vec::new();
    for result_set in result_sets {
        for result in &result_set.results {
            if !versions.contains(&result.version) {
                versions.push(result.version.clone());
            }
        }
    }

    print!("{:<12}", "URL");
    for version in &versions {
        print!(" {version:>12}");
    }
    println!();
    println!("{}", "-".repeat(12 + 13 * versions.len()));

    for result_set in result_sets {
        print!("{:<12}", result_set.url_name);
        for version in &versions {
            let time = result_set
                .results
                .iter()
                .find(|r| r.version == *version)
                .map(|r| format!("{:.3}s", r.mean.as_secs_f64()))
                .unwrap_or_else(|| "-".to_string());
            print!(" {time:>12}");
        }
        println!();
    }
    println!();
}

/// Outputs results for multiple URLs as JSON.
pub(super) fn output_json_multi(result_sets: &[BenchmarkResultSet]) -> TaskResult<()> {
    println!("{{");
    println!("  \"benchmarks\": [");
    for (i, result_set) in result_sets.iter().enumerate() {
        let outer_comma = if i < result_sets.len() - 1 { "," } else { "" };
        println!("    {{");
        println!("      \"url\": \"{}\",", result_set.url);
        println!("      \"url_name\": \"{}\",", result_set.url_name);
        println!("      \"results\": [");
        for (j, result) in result_set.results.iter().enumerate() {
            let comma = if j < result_set.results.len() - 1 {
                ","
            } else {
                ""
            };
            let total_bytes: u64 = result.samples.iter().map(|s| s.bytes_received).sum();
            let total_size: u64 = result.samples.last().map(|s| s.total_size).unwrap_or(0);
            println!("        {{");
            println!("          \"version\": \"{}\",", result.version);
            println!(
                "          \"mean_ms\": {:.3},",
                result.mean.as_secs_f64() * 1000.0
            );
            println!(
                "          \"min_ms\": {:.3},",
                result.min.as_secs_f64() * 1000.0
            );
            println!(
                "          \"max_ms\": {:.3},",
                result.max.as_secs_f64() * 1000.0
            );
            println!("          \"stddev_ms\": {:.3},", result.stddev * 1000.0);
            println!(
                "          \"throughput_mbps\": {:.3},",
                result.mean_throughput_mbps
            );
            println!("          \"total_bytes_received\": {total_bytes},");
            println!("          \"total_file_size\": {total_size},");
            println!(
                "          \"runs_ms\": [{}]",
                result
                    .runs
                    .iter()
                    .map(|d| format!("{:.3}", d.as_secs_f64() * 1000.0))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            println!("        }}{comma}");
        }
        println!("      ]");
        println!("    }}{outer_comma}");
    }
    println!("  ]");
    println!("}}");
    Ok(())
}

/// Outputs loopback results as formatted tables.
pub(super) fn output_loopback_table(
    client_results: &[BenchmarkResult],
    server_results: &[BenchmarkResult],
) {
    println!("\n=== Loopback Benchmark Results ===");

    println!("\n--- Client Performance (version as client -> dev reference daemon) ---\n");
    print_result_table(client_results);

    if !server_results.is_empty() {
        println!("--- Server Performance (dev client -> version as daemon) ---\n");
        print_result_table(server_results);
    }
}

/// Prints a formatted table for a set of benchmark results.
fn print_result_table(results: &[BenchmarkResult]) {
    println!(
        "{:<12} {:>10} {:>10} {:>10} {:>10} {:>12}",
        "Version", "Mean", "Min", "Max", "Stddev", "Throughput"
    );
    println!("{}", "-".repeat(68));

    let baseline = results.first().map(|r| r.mean).unwrap_or(Duration::ZERO);

    for result in results {
        let diff_pct = if baseline > Duration::ZERO {
            ((result.mean.as_secs_f64() - baseline.as_secs_f64()) / baseline.as_secs_f64()) * 100.0
        } else {
            0.0
        };

        let speedup_str = if diff_pct.abs() < 1.0 {
            String::new()
        } else if diff_pct > 0.0 {
            format!(" ({diff_pct:.1}% slower)")
        } else {
            let faster = -diff_pct;
            format!(" ({faster:.1}% faster)")
        };

        let throughput_str = if result.mean_throughput_mbps > 0.0 {
            format!("{:.2} MB/s", result.mean_throughput_mbps)
        } else {
            "-".to_string()
        };

        println!(
            "{:<12} {:>10.3}s {:>10.3}s {:>10.3}s {:>10.4}s {:>12}{}",
            result.version,
            result.mean.as_secs_f64(),
            result.min.as_secs_f64(),
            result.max.as_secs_f64(),
            result.stddev,
            throughput_str,
            speedup_str
        );
    }
    println!();
}

/// Outputs loopback results as JSON.
pub(super) fn output_loopback_json(
    client_results: &[BenchmarkResult],
    server_results: &[BenchmarkResult],
) -> TaskResult<()> {
    println!("{{");
    println!("  \"mode\": \"loopback\",");

    println!("  \"client_benchmarks\": [");
    print_json_results(client_results);
    println!("  ],");

    println!("  \"server_benchmarks\": [");
    print_json_results(server_results);
    println!("  ]");

    println!("}}");
    Ok(())
}

/// Prints benchmark results as JSON array elements.
fn print_json_results(results: &[BenchmarkResult]) {
    for (j, result) in results.iter().enumerate() {
        let comma = if j < results.len() - 1 { "," } else { "" };
        let total_bytes: u64 = result.samples.iter().map(|s| s.bytes_received).sum();
        let total_size: u64 = result.samples.last().map(|s| s.total_size).unwrap_or(0);
        println!("    {{");
        println!("      \"version\": \"{}\",", result.version);
        println!(
            "      \"mean_ms\": {:.3},",
            result.mean.as_secs_f64() * 1000.0
        );
        println!(
            "      \"min_ms\": {:.3},",
            result.min.as_secs_f64() * 1000.0
        );
        println!(
            "      \"max_ms\": {:.3},",
            result.max.as_secs_f64() * 1000.0
        );
        println!("      \"stddev_ms\": {:.3},", result.stddev * 1000.0);
        println!(
            "      \"throughput_mbps\": {:.3},",
            result.mean_throughput_mbps
        );
        println!("      \"total_bytes_received\": {total_bytes},");
        println!("      \"total_file_size\": {total_size},");
        println!(
            "      \"runs_ms\": [{}]",
            result
                .runs
                .iter()
                .map(|d| format!("{:.3}", d.as_secs_f64() * 1000.0))
                .collect::<Vec<_>>()
                .join(", ")
        );
        println!("    }}{comma}");
    }
}
