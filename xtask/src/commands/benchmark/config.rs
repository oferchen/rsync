//! Configuration types and constants for the benchmark command.
//!
//! Holds workload parameters, the CLI-derived options struct, and the
//! static catalogue of public rsync mirrors used in remote mode.

use crate::cli::{BenchmarkArgs, BenchmarkMode, DataProfile};
use std::path::PathBuf;

/// Default benchmark data directory.
const DEFAULT_BENCH_DIR: &str = "/tmp/rsync-bench";

/// Default rsync daemon port.
pub(super) const DEFAULT_DAEMON_PORT: u16 = 8873;

/// Number of benchmark runs per version.
pub(super) const DEFAULT_RUNS: usize = 5;

/// Default base port for loopback mode daemons.
pub(super) const DEFAULT_LOOPBACK_PORT: u16 = 18873;

/// Public rsync mirrors for remote benchmarking (not kernel.org).
///
/// Selected for diverse geography, file profiles, and practical transfer sizes.
pub(super) const REMOTE_MIRRORS: &[RemoteMirror] = &[
    RemoteMirror {
        name: "GNU-hello",
        url: "rsync://ftp.gnu.org/gnu/hello/",
        description: "GNU hello - 44 files, ~12MB (US)",
    },
    RemoteMirror {
        name: "GNU-which",
        url: "rsync://ftp.gnu.org/gnu/which/",
        description: "GNU which - 15 files, ~1MB (US, connection overhead test)",
    },
    RemoteMirror {
        name: "Apache",
        url: "rsync://rsync.apache.org/apache-dist/httpd/",
        description: "Apache HTTPD dist - 55 files, ~35MB (US)",
    },
    RemoteMirror {
        name: "Berkeley",
        url: "rsync://mirrors.ocf.berkeley.edu/gnu/findutils/",
        description: "UC Berkeley GNU findutils - 47 files, ~34MB (US West)",
    },
    RemoteMirror {
        name: "CTAN",
        url: "rsync://rsync.dante.ctan.org/CTAN/macros/latex/base/",
        description: "CTAN LaTeX base - 314 files, ~54MB (Germany)",
    },
    RemoteMirror {
        name: "CPAN",
        url: "rsync://cpan-rsync.perl.org/CPAN/modules/by-module/HTTP/",
        description: "CPAN HTTP modules - varied sizes (US)",
    },
    RemoteMirror {
        name: "RIT-Arch",
        url: "rsync://mirrors.rit.edu/archlinux/core/os/x86_64/",
        description: "RIT Arch Linux core - ~400 files, ~300MB (US East)",
    },
    RemoteMirror {
        name: "Princeton",
        url: "rsync://mirror.math.princeton.edu/pub/slackware/slackware64-current/ChangeLog.txt",
        description: "Princeton Math single file - connection latency test (US East, Internet2)",
    },
    RemoteMirror {
        name: "OSUOSL",
        url: "rsync://rsync.osuosl.org/ubuntu/dists/noble/Release",
        description: "OSUOSL Ubuntu release file - connection test (US West)",
    },
];

/// A remote rsync mirror for benchmarking.
#[derive(Debug, Clone, Copy)]
pub(super) struct RemoteMirror {
    pub(super) name: &'static str,
    pub(super) url: &'static str,
    pub(super) description: &'static str,
}

/// Benchmark configuration options.
#[derive(Clone, Debug)]
pub struct BenchmarkOptions {
    /// Directory for benchmark data and daemon.
    pub bench_dir: PathBuf,
    /// Rsync daemon port.
    pub port: u16,
    /// Number of runs per version.
    pub runs: usize,
    /// Versions to benchmark (empty = auto-detect).
    pub versions: Vec<String>,
    /// Skip building versions (use existing binaries).
    pub skip_build: bool,
    /// Output format.
    pub json: bool,
    /// Benchmark mode: local or remote.
    pub mode: BenchmarkMode,
    /// Custom remote URLs for benchmarking.
    pub urls: Vec<String>,
    /// List available mirrors and exit.
    pub list_mirrors: bool,
    /// Data profile for loopback mode test data generation.
    pub data_profile: DataProfile,
}

impl Default for BenchmarkOptions {
    fn default() -> Self {
        Self {
            bench_dir: PathBuf::from(DEFAULT_BENCH_DIR),
            port: DEFAULT_DAEMON_PORT,
            runs: DEFAULT_RUNS,
            versions: Vec::new(),
            skip_build: false,
            json: false,
            mode: BenchmarkMode::Local,
            urls: Vec::new(),
            list_mirrors: false,
            data_profile: DataProfile::Medium,
        }
    }
}

impl From<BenchmarkArgs> for BenchmarkOptions {
    fn from(args: BenchmarkArgs) -> Self {
        Self {
            bench_dir: args
                .bench_dir
                .unwrap_or_else(|| PathBuf::from(DEFAULT_BENCH_DIR)),
            port: args.port.unwrap_or(DEFAULT_DAEMON_PORT),
            runs: args.runs.unwrap_or(DEFAULT_RUNS),
            versions: args.versions,
            skip_build: args.skip_build,
            json: args.json,
            mode: args.mode,
            urls: args.urls,
            list_mirrors: args.list_mirrors,
            data_profile: args.data_profile,
        }
    }
}

/// Returns the file count and directory count for a given data profile.
pub(super) fn profile_params(profile: DataProfile) -> (usize, usize) {
    match profile {
        DataProfile::Small => (1_000, 10),
        DataProfile::Medium => (10_000, 100),
        DataProfile::Large => (50_000, 500),
    }
}

/// Extracts a short name from a URL for display.
pub(super) fn url_short_name(url: &str) -> String {
    for mirror in REMOTE_MIRRORS {
        if url == mirror.url {
            return mirror.name.to_string();
        }
    }
    url.strip_prefix("rsync://")
        .and_then(|s| s.split('/').next())
        .unwrap_or("unknown")
        .to_string()
}

/// Lists available public rsync mirrors to stdout.
pub(super) fn list_mirrors() -> crate::error::TaskResult<()> {
    println!("=== Available Public Rsync Mirrors ===\n");
    println!("{:<12} {:<55} Description", "Name", "URL");
    println!("{}", "-".repeat(100));
    for mirror in REMOTE_MIRRORS {
        println!(
            "{:<12} {:<55} {}",
            mirror.name, mirror.url, mirror.description
        );
    }
    println!("\nUsage: cargo xtask benchmark --mode remote [--url <custom-url>]");
    Ok(())
}
