//! Loopback benchmark mode: per-version daemons and client/server measurements.

use crate::cli::DataProfile;
use crate::error::{TaskError, TaskResult};
use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use super::config::{BenchmarkOptions, DEFAULT_LOOPBACK_PORT, profile_params};
use super::report::{output_loopback_json, output_loopback_table};
use super::runner::{
    BenchmarkResult, build_release, build_version, detect_versions, find_upstream_rsync,
    run_benchmark,
};

/// RAII guard that kills a daemon process and removes its pid file on drop.
struct DaemonGuard {
    #[allow(dead_code)]
    name: String,
    child: Option<Child>,
    pid_file: PathBuf,
    conf_file: PathBuf,
}

impl DaemonGuard {
    fn new(name: String, child: Child, pid_file: PathBuf, conf_file: PathBuf) -> Self {
        Self {
            name,
            child: Some(child),
            pid_file,
            conf_file,
        }
    }
}

impl Drop for DaemonGuard {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        let _ = fs::remove_file(&self.pid_file);
        let _ = fs::remove_file(&self.conf_file);
    }
}

/// Generates deterministic test data for loopback benchmarks.
///
/// Creates a mix of file sizes in nested directories using a seeded PRNG
/// for reproducibility. Skips generation if a marker file indicates the
/// requested profile already exists.
pub(super) fn generate_loopback_data(
    bench_dir: &Path,
    profile: DataProfile,
) -> TaskResult<PathBuf> {
    let data_dir = bench_dir.join("loopback-data");
    let marker = data_dir.join(".profile");

    let profile_name = format!("{profile:?}");
    if marker.exists() {
        if let Ok(existing) = fs::read_to_string(&marker) {
            if existing.trim() == profile_name {
                println!(
                    "Reusing existing {profile_name} test data in {}",
                    data_dir.display()
                );
                return Ok(data_dir);
            }
        }
    }

    let _ = fs::remove_dir_all(&data_dir);
    fs::create_dir_all(&data_dir)?;

    let (file_count, dir_count) = profile_params(profile);
    println!("Generating {profile_name} test data: {file_count} files in {dir_count} dirs...");

    // Build a two-level directory structure to spread file_count across dir_count buckets.
    let dirs_per_level = (dir_count as f64).sqrt().ceil() as usize;
    let mut dir_paths: Vec<PathBuf> = Vec::with_capacity(dir_count);
    for i in 0..dirs_per_level {
        let level1 = data_dir.join(format!("d{i:04}"));
        fs::create_dir_all(&level1)?;
        dir_paths.push(level1.clone());
        for j in 0..dirs_per_level {
            if dir_paths.len() >= dir_count {
                break;
            }
            let level2 = level1.join(format!("s{j:04}"));
            fs::create_dir_all(&level2)?;
            dir_paths.push(level2);
        }
        if dir_paths.len() >= dir_count {
            break;
        }
    }

    // Simple seeded PRNG (xorshift64)
    let mut rng_state: u64 = 0xDEAD_BEEF_CAFE_BABE;
    let mut next_rand = move || -> u64 {
        rng_state ^= rng_state << 13;
        rng_state ^= rng_state >> 7;
        rng_state ^= rng_state << 17;
        rng_state
    };

    // Generate files with varied sizes:
    // 5% empty, 30% tiny (<1KB), 40% small (1-10KB), 20% medium (10-100KB), 5% large (100KB-1MB)
    let mut buf = vec![0u8; 1024 * 1024]; // 1MB reusable buffer
    for i in 0..file_count {
        let dir = &dir_paths[i % dir_paths.len()];
        let file_path = dir.join(format!("f{i:06}.dat"));

        let r = (next_rand() % 100) as u32;
        let size = match r {
            0..5 => 0,                                                  // empty
            5..35 => (next_rand() % 1024) as usize,                     // tiny: 0-1023
            35..75 => 1024 + (next_rand() % (9 * 1024)) as usize,       // small: 1KB-10KB
            75..95 => 10 * 1024 + (next_rand() % (90 * 1024)) as usize, // medium: 10-100KB
            _ => 100 * 1024 + (next_rand() % (900 * 1024)) as usize,    // large: 100KB-1MB
        };

        if size == 0 {
            fs::File::create(&file_path)?;
        } else {
            for chunk in buf[..size].chunks_mut(8) {
                let val = next_rand();
                let bytes = val.to_le_bytes();
                let copy_len = chunk.len().min(8);
                chunk[..copy_len].copy_from_slice(&bytes[..copy_len]);
            }
            let mut f = fs::File::create(&file_path)?;
            f.write_all(&buf[..size])?;
        }

        if (i + 1) % 5000 == 0 {
            println!("  Generated {}/{file_count} files...", i + 1);
        }
    }

    fs::write(&marker, &profile_name)?;

    let total_bytes: u64 = walkdir_size(&data_dir);
    println!(
        "Generated {file_count} files ({:.1} MB) in {}",
        total_bytes as f64 / (1024.0 * 1024.0),
        data_dir.display()
    );

    Ok(data_dir)
}

/// Calculates the total size of all files in a directory tree.
pub(super) fn walkdir_size(dir: &Path) -> u64 {
    let mut total = 0u64;
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                total += walkdir_size(&path);
            } else if let Ok(meta) = path.metadata() {
                total += meta.len();
            }
        }
    }
    total
}

/// Waits for a TCP port to become reachable on localhost.
fn wait_for_port(port: u16, timeout: Duration) -> bool {
    use std::net::TcpStream;
    let start = Instant::now();
    while start.elapsed() < timeout {
        if TcpStream::connect(format!("127.0.0.1:{port}")).is_ok() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    false
}

/// Checks that a port is available for binding.
pub(super) fn port_available(port: u16) -> bool {
    use std::net::TcpListener;
    TcpListener::bind(format!("127.0.0.1:{port}")).is_ok()
}

/// Starts a daemon from the given binary on the specified port.
///
/// Returns a `DaemonGuard` that kills the daemon on drop.
fn start_loopback_daemon(
    name: &str,
    binary: &Path,
    data_dir: &Path,
    port: u16,
    bench_dir: &Path,
    is_oc_rsync: bool,
) -> TaskResult<DaemonGuard> {
    let conf_path = bench_dir.join(format!("loopback-{name}.conf"));
    let pid_path = bench_dir.join(format!("loopback-{name}.pid"));
    let log_path = bench_dir.join(format!("loopback-{name}.log"));

    // Write daemon config: global directives before any [module] section
    let config = if is_oc_rsync {
        format!(
            r#"pid file = {pid}
log file = {log}
port = {port}
use chroot = false

[bench]
path = {data}
comment = Loopback benchmark data
read only = true
"#,
            pid = pid_path.display(),
            log = log_path.display(),
            data = data_dir.display(),
        )
    } else {
        format!(
            r#"port = {port}
pid file = {pid}
log file = {log}
use chroot = no
read only = yes

[bench]
    path = {data}
    comment = Loopback benchmark data
"#,
            pid = pid_path.display(),
            log = log_path.display(),
            data = data_dir.display(),
        )
    };
    fs::write(&conf_path, &config)?;

    // Start daemon
    let mut cmd = Command::new(binary);
    cmd.args(["--daemon", "--no-detach", "--config"])
        .arg(&conf_path)
        .arg("--port")
        .arg(port.to_string())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());

    // Force native daemon mode for oc-rsync (no fallback to system rsync)
    if is_oc_rsync {
        cmd.env("OC_RSYNC_DAEMON_FALLBACK", "0");
    }

    let child = cmd.spawn().map_err(|e| {
        TaskError::Validation(format!("failed to start {name} daemon on port {port}: {e}"))
    })?;

    let guard = DaemonGuard::new(name.to_string(), child, pid_path, conf_path);

    // Wait for daemon to be ready
    if !wait_for_port(port, Duration::from_secs(5)) {
        return Err(TaskError::Validation(format!(
            "{name} daemon failed to start on port {port} within 5 seconds"
        )));
    }

    println!("  Started {name} daemon on port {port}");
    Ok(guard)
}

/// Tries to start a daemon, returning `Ok(None)` on failure instead of an error.
fn try_start_loopback_daemon(
    name: &str,
    binary: &Path,
    data_dir: &Path,
    port: u16,
    bench_dir: &Path,
    is_oc_rsync: bool,
) -> TaskResult<Option<DaemonGuard>> {
    match start_loopback_daemon(name, binary, data_dir, port, bench_dir, is_oc_rsync) {
        Ok(guard) => Ok(Some(guard)),
        Err(e) => {
            eprintln!("  Warning: {name} daemon failed to start: {e}");
            Ok(None)
        }
    }
}

/// Orchestrates the full loopback benchmark flow.
///
/// Uses oc-rsync dev as the reference daemon for client benchmarks, since the
/// system rsync may be openrsync (macOS) which has daemon compatibility issues.
/// Each oc-rsync version also runs as a daemon for server benchmarks, tested
/// with the dev client.
pub(super) fn run_loopback_benchmarks(
    workspace: &Path,
    options: &BenchmarkOptions,
) -> TaskResult<()> {
    let base_port = options.port.max(DEFAULT_LOOPBACK_PORT);

    println!("=== Loopback Benchmark Mode ===\n");
    println!("Profile: {:?}", options.data_profile);
    println!("Runs: {}", options.runs);
    println!("Base port: {base_port}\n");

    // Step 1: Generate test data
    let data_dir = generate_loopback_data(&options.bench_dir, options.data_profile)?;

    // Step 2: Determine versions and build binaries
    let versions = if options.versions.is_empty() {
        detect_versions(workspace)?
    } else {
        options.versions.clone()
    };
    println!("\nVersions to benchmark: {versions:?}");

    let mut binaries: HashMap<String, PathBuf> = HashMap::new();

    // Upstream rsync (for client benchmarks only - may be openrsync)
    if let Some(rsync_path) = find_upstream_rsync() {
        binaries.insert("upstream".to_string(), rsync_path);
    }

    // Current dev build
    if !options.skip_build {
        println!("\nBuilding current development version...");
        build_release(workspace)?;
    }
    let current_binary = workspace.join("target/release/oc-rsync");
    if current_binary.exists() {
        binaries.insert("dev".to_string(), current_binary);
    }

    // Tagged versions
    for version in &versions {
        if version == "dev" || version == "upstream" {
            continue;
        }
        let binary_path = options.bench_dir.join(format!("oc-rsync-{version}"));
        if !binary_path.exists() && !options.skip_build {
            println!("Building version {version}...");
            if let Err(e) = build_version(workspace, version, &binary_path) {
                eprintln!("Warning: failed to build {version}: {e}");
                continue;
            }
        }
        if binary_path.exists() {
            binaries.insert(version.clone(), binary_path);
        }
    }

    if binaries.is_empty() {
        return Err(TaskError::Validation(
            "no binaries available for benchmarking".into(),
        ));
    }

    // We need a dev binary for the reference daemon
    let dev_binary = binaries.get("dev").cloned();
    if dev_binary.is_none() {
        return Err(TaskError::Validation(
            "dev binary required for loopback mode (reference daemon)".into(),
        ));
    }
    let dev_binary = dev_binary.unwrap();

    // Collect oc-rsync version names (non-upstream) for per-version daemons
    let mut oc_versions: Vec<String> = binaries
        .keys()
        .filter(|k| *k != "upstream")
        .cloned()
        .collect();
    oc_versions.sort();

    // Step 3: Verify ports are available and allocate
    // Port layout: base_port = reference daemon (dev), then one per oc-rsync version
    let reference_port = base_port;
    if !port_available(reference_port) {
        return Err(TaskError::Validation(format!(
            "port {reference_port} is already in use"
        )));
    }

    let mut version_ports: HashMap<String, u16> = HashMap::new();
    for (idx, version) in oc_versions.iter().enumerate() {
        let port = base_port + 2 * (idx as u16 + 1);
        if !port_available(port) {
            return Err(TaskError::Validation(format!(
                "port {port} (for {version} daemon) is already in use"
            )));
        }
        version_ports.insert(version.clone(), port);
    }

    // Step 4: Start daemons
    println!("\nStarting daemons...");
    let mut guards: Vec<DaemonGuard> = Vec::new();

    // Start reference daemon (dev) for client benchmarks
    let ref_guard = start_loopback_daemon(
        "ref-dev",
        &dev_binary,
        &data_dir,
        reference_port,
        &options.bench_dir,
        true,
    )?;
    guards.push(ref_guard);

    // Start per-version daemons for server benchmarks
    let mut daemon_started: HashMap<String, bool> = HashMap::new();
    for version in &oc_versions {
        let port = version_ports[version];
        let binary = &binaries[version];
        match try_start_loopback_daemon(version, binary, &data_dir, port, &options.bench_dir, true)?
        {
            Some(guard) => {
                guards.push(guard);
                daemon_started.insert(version.clone(), true);
            }
            None => {
                daemon_started.insert(version.clone(), false);
            }
        }
    }

    let dest_dir = options.bench_dir.join("loopback-dest");
    let client_url = format!("rsync://127.0.0.1:{reference_port}/bench/");

    // Step 5: Warmup
    println!("\nRunning warmup...");
    for (version, binary) in &binaries {
        let _ = fs::remove_dir_all(&dest_dir);
        fs::create_dir_all(&dest_dir)?;
        let output = Command::new(binary)
            .args(["-a", &client_url, dest_dir.to_str().unwrap()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .output();
        match output {
            Ok(o) if o.status.success() => println!("  Warmup OK: {version}"),
            _ => println!("  Warmup skipped: {version} (client connection failed)"),
        }
    }

    // Step 6: Client benchmarks (each version's client -> dev reference daemon)
    println!("\n=== Client Benchmarks (version as client -> dev reference daemon) ===");
    let mut client_results: Vec<BenchmarkResult> = Vec::new();

    for (version, binary) in &binaries {
        println!("\nBenchmarking {version} (client)...");
        let samples = run_benchmark(binary, &client_url, &dest_dir, options.runs)?;
        if samples.is_empty() {
            eprintln!("  Skipping {version} - all runs failed");
            continue;
        }
        client_results.push(BenchmarkResult::new(version.clone(), samples));
    }
    client_results.sort_by(|a, b| a.mean.cmp(&b.mean));

    // Step 7: Server benchmarks (dev client -> each version's daemon)
    println!("\n=== Server Benchmarks (dev client -> version as daemon) ===");
    let mut server_results: Vec<BenchmarkResult> = Vec::new();

    for version in &oc_versions {
        if !daemon_started.get(version).copied().unwrap_or(false) {
            continue;
        }
        let port = version_ports[version];
        let server_url = format!("rsync://127.0.0.1:{port}/bench/");
        println!("\nBenchmarking {version} daemon (server)...");
        let samples = run_benchmark(&dev_binary, &server_url, &dest_dir, options.runs)?;
        if samples.is_empty() {
            eprintln!("  Skipping {version} daemon - all runs failed");
            continue;
        }
        server_results.push(BenchmarkResult::new(version.clone(), samples));
    }
    server_results.sort_by(|a, b| a.mean.cmp(&b.mean));

    // Step 8: Output results
    if options.json {
        output_loopback_json(&client_results, &server_results)?;
    } else {
        output_loopback_table(&client_results, &server_results);
    }

    // Guards are dropped here, killing all daemons
    println!("\nStopping daemons...");
    drop(guards);
    println!("Done.");

    Ok(())
}
