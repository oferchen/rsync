//! Integration test helpers for CLI-level end-to-end testing.
//!
//! Provides utilities for:
//! - Temporary directory management with automatic cleanup
//! - File tree creation and comparison
//! - Binary execution and output capture
//! - Metadata verification

#![allow(dead_code)] // Helpers will be used by integration test files

use std::collections::{HashMap, HashSet};
use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// Test directory with automatic cleanup on drop.
pub struct TestDir {
    path: PathBuf,
}

impl TestDir {
    /// Create a new temporary test directory.
    pub fn new() -> io::Result<Self> {
        let mut base = env::temp_dir();
        base.push("rsync_integration_tests");
        fs::create_dir_all(&base)?;

        for attempt in 0..100 {
            let name = format!(
                "test_{}_{}_{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos(),
                attempt
            );
            let candidate = base.join(name);
            match fs::create_dir(&candidate) {
                Ok(()) => return Ok(Self { path: candidate }),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e),
            }
        }

        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "failed to create unique test directory",
        ))
    }

    /// Get the path to this test directory.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Create a subdirectory within this test directory.
    pub fn mkdir(&self, name: &str) -> io::Result<PathBuf> {
        let dir = self.path.join(name);
        fs::create_dir_all(&dir)?;
        Ok(dir)
    }

    /// Create a file with given content.
    pub fn write_file(&self, rel_path: &str, content: &[u8]) -> io::Result<PathBuf> {
        let path = self.path.join(rel_path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&path, content)?;
        Ok(path)
    }

    /// Read a file's content.
    pub fn read_file(&self, rel_path: &str) -> io::Result<Vec<u8>> {
        fs::read(self.path.join(rel_path))
    }

    /// Check if a file exists.
    pub fn exists(&self, rel_path: &str) -> bool {
        self.path.join(rel_path).exists()
    }

    /// List all files recursively (relative paths).
    pub fn list_files(&self) -> io::Result<Vec<PathBuf>> {
        let mut files = Vec::new();
        collect_files_for_listing(&self.path, &self.path, &mut files)?;
        files.sort();
        Ok(files)
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

/// Binary command builder for integration tests.
pub struct RsyncCommand {
    binary: PathBuf,
    args: Vec<String>,
}

impl RsyncCommand {
    /// Create a new command for the oc-rsync binary.
    pub fn new() -> Self {
        let binary = locate_binary("oc-rsync")
            .expect("oc-rsync binary must be available for integration tests");
        Self {
            binary,
            args: Vec::new(),
        }
    }

    /// Add an argument.
    pub fn arg<S: AsRef<str>>(&mut self, arg: S) -> &mut Self {
        self.args.push(arg.as_ref().to_owned());
        self
    }

    /// Add multiple arguments.
    pub fn args<I, S>(&mut self, args: I) -> &mut Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        for arg in args {
            self.arg(arg);
        }
        self
    }

    /// Execute the command and return the output.
    pub fn run(&self) -> io::Result<Output> {
        let mut command = if let Some(runner) = cargo_target_runner() {
            let mut cmd = Command::new(&runner[0]);
            cmd.args(&runner[1..]);
            cmd.arg(&self.binary);
            cmd
        } else {
            Command::new(&self.binary)
        };

        command.args(&self.args);
        command.output()
    }

    /// Execute and assert success.
    pub fn assert_success(&self) -> Output {
        let output = self.run().expect("command execution failed");
        if !output.status.success() {
            eprintln!("=== Command failed ===");
            eprintln!("Binary: {:?}", self.binary);
            eprintln!("Args: {:?}", self.args);
            eprintln!("Exit code: {:?}", output.status.code());
            eprintln!("=== stdout ===");
            eprintln!("{}", String::from_utf8_lossy(&output.stdout));
            eprintln!("=== stderr ===");
            eprintln!("{}", String::from_utf8_lossy(&output.stderr));
            panic!("command should have succeeded");
        }
        output
    }

    /// Execute and assert failure.
    pub fn assert_failure(&self) -> Output {
        let output = self.run().expect("command execution failed");
        if output.status.success() {
            eprintln!("=== Command unexpectedly succeeded ===");
            eprintln!("Binary: {:?}", self.binary);
            eprintln!("Args: {:?}", self.args);
            eprintln!("=== stdout ===");
            eprintln!("{}", String::from_utf8_lossy(&output.stdout));
            eprintln!("=== stderr ===");
            eprintln!("{}", String::from_utf8_lossy(&output.stderr));
            panic!("command should have failed");
        }
        output
    }
}

/// Locate the test binary.
fn locate_binary(name: &str) -> Option<PathBuf> {
    // Try CARGO_BIN_EXE_<name> first
    let env_var = format!("CARGO_BIN_EXE_{name}");
    if let Some(path) = env::var_os(&env_var) {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Some(path);
        }
    }

    // Try to find in target directory
    let current_exe = env::current_exe().ok()?;
    let mut dir = current_exe.parent()?;

    // Walk up to find target directory
    while !dir.ends_with("target") {
        dir = dir.parent()?;
    }

    // Check common locations
    let binary_name = format!("{name}{}", std::env::consts::EXE_SUFFIX);
    for subdir in ["debug", "release"] {
        let candidate = dir.join(subdir).join(&binary_name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }

    None
}

/// Get cargo target runner if configured.
fn cargo_target_runner() -> Option<Vec<String>> {
    let target = env::var("TARGET").ok()?;
    let runner_env = format!(
        "CARGO_TARGET_{}_RUNNER",
        target.replace('-', "_").to_uppercase()
    );
    let runner = env::var(&runner_env).ok()?;

    if runner.trim().is_empty() {
        return None;
    }

    // Simple split on whitespace (doesn't handle quoting)
    let words: Vec<String> = runner.split_whitespace().map(String::from).collect();
    if words.is_empty() { None } else { Some(words) }
}

/// File tree builder for creating test fixtures.
pub struct FileTree {
    files: HashMap<String, Vec<u8>>,
}

impl FileTree {
    /// Create a new empty file tree.
    pub fn new() -> Self {
        Self {
            files: HashMap::new(),
        }
    }

    /// Add a file with content.
    pub fn file<S: Into<String>>(&mut self, path: S, content: &[u8]) -> &mut Self {
        self.files.insert(path.into(), content.to_vec());
        self
    }

    /// Add a text file.
    pub fn text_file<S: Into<String>>(&mut self, path: S, content: &str) -> &mut Self {
        self.file(path, content.as_bytes())
    }

    /// Create all files in the given directory.
    pub fn create_in(&self, dir: &TestDir) -> io::Result<()> {
        for (path, content) in &self.files {
            dir.write_file(path, content)?;
        }
        Ok(())
    }
}

impl Default for FileTree {
    fn default() -> Self {
        Self::new()
    }
}

/// Compare two directories for equality.
pub fn assert_dirs_equal(left: &Path, right: &Path) {
    let left_files = collect_file_set(left).expect("failed to list left directory");
    let right_files = collect_file_set(right).expect("failed to list right directory");

    let mut errors = Vec::new();

    // Check for files in left but not right
    for file in left_files.difference(&right_files) {
        errors.push(format!(
            "File exists in source but not dest: {}",
            file.display()
        ));
    }

    // Check for files in right but not left
    for file in right_files.difference(&left_files) {
        errors.push(format!(
            "File exists in dest but not source: {}",
            file.display()
        ));
    }

    // Compare content of common files
    for file in &left_files {
        let left_content = fs::read(left.join(file)).expect("failed to read source file");
        let right_content = fs::read(right.join(file)).expect("failed to read dest file");

        if left_content != right_content {
            errors.push(format!(
                "File content differs: {}\n  Source: {} bytes\n  Dest: {} bytes",
                file.display(),
                left_content.len(),
                right_content.len()
            ));
        }
    }

    if !errors.is_empty() {
        panic!("Directory comparison failed:\n{}", errors.join("\n"));
    }
}

fn collect_files_for_listing(
    base: &Path,
    current: &Path,
    files: &mut Vec<PathBuf>,
) -> io::Result<()> {
    for entry in fs::read_dir(current)? {
        let entry = entry?;
        let path = entry.path();
        let rel_path = path.strip_prefix(base).unwrap().to_path_buf();

        if path.is_file() {
            files.push(rel_path);
        } else if path.is_dir() {
            collect_files_for_listing(base, &path, files)?;
        }
    }
    Ok(())
}

fn collect_file_set(base: &Path) -> io::Result<HashSet<PathBuf>> {
    let mut files = HashSet::new();
    collect_files_recursive(base, base, &mut files)?;
    Ok(files)
}

fn collect_files_recursive(
    base: &Path,
    current: &Path,
    files: &mut HashSet<PathBuf>,
) -> io::Result<()> {
    for entry in fs::read_dir(current)? {
        let entry = entry?;
        let path = entry.path();

        if path.is_file() {
            let rel_path = path.strip_prefix(base).unwrap().to_path_buf();
            files.insert(rel_path);
        } else if path.is_dir() {
            collect_files_recursive(base, &path, files)?;
        }
    }
    Ok(())
}

// ============ Protocol Version Testing Infrastructure ============

/// Test infrastructure for server-mode protocol testing.
///
/// This enables testing oc-rsync in `--server` mode against upstream rsync clients.
/// Protocol version is determined by the maximum version both sides support:
/// - rsync 3.0.9 → protocol 30 (MD5 checksums)
/// - rsync 3.1.3 → protocol 31 (MD5 checksums)
/// - rsync 3.4.1 → protocol 32 (MD5/XXH3 checksums)
///
/// Note: Protocol 28-29 (MD4 checksums) would require older rsync versions
/// like rsync 2.6.x which are not commonly available.
pub struct ServerModeTest {
    oc_rsync_binary: PathBuf,
    upstream_binary: PathBuf,
}

impl ServerModeTest {
    /// Create a new server-mode test with specified upstream rsync binary.
    pub fn new(upstream_binary: &Path) -> Option<Self> {
        let oc_rsync_binary = locate_binary("oc-rsync")?;
        if !upstream_binary.is_file() {
            return None;
        }
        Some(Self {
            oc_rsync_binary,
            upstream_binary: upstream_binary.to_path_buf(),
        })
    }

    /// Run a push transfer: upstream rsync client (sender) → oc-rsync server (receiver).
    ///
    /// The upstream rsync initiates a transfer to oc-rsync running in server mode.
    /// Protocol version is negotiated based on the upstream version's maximum.
    pub fn push_transfer(&self, source: &Path, dest: &Path) -> io::Result<TransferResult> {
        use std::io::{Read, Write};
        use std::process::Stdio;
        use std::thread;

        // Spawn oc-rsync in --server mode (receiver)
        // Flag string format: -vlogDtprze.iLsfxCIvu
        let mut server = Command::new(&self.oc_rsync_binary)
            .arg("--server")
            .arg("-vlogDtprze.iLsfxCIvu")
            .arg(".")
            .arg(dest.to_string_lossy().as_ref())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        // Spawn upstream rsync in --server --sender mode
        // This simulates the remote server that normally runs via SSH
        let mut client = Command::new(&self.upstream_binary)
            .arg("--server")
            .arg("--sender")
            .arg("-vlogDtprze.iLsfxCIvu")
            .arg(".")
            .arg(source.to_string_lossy().as_ref())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        // Wire up bidirectional pipes between processes
        let client_stdout = client.stdout.take().unwrap();
        let client_stdin = client.stdin.take().unwrap();
        let server_stdout = server.stdout.take().unwrap();
        let server_stdin = server.stdin.take().unwrap();

        // Spawn threads to copy data in both directions
        let c2s = thread::spawn(move || -> io::Result<()> {
            let mut reader = std::io::BufReader::new(client_stdout);
            let mut writer = std::io::BufWriter::new(server_stdin);
            let mut buf = [0u8; 4096];
            loop {
                let n = reader.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                writer.write_all(&buf[..n])?;
                writer.flush()?;
            }
            Ok(())
        });

        let s2c = thread::spawn(move || -> io::Result<()> {
            let mut reader = std::io::BufReader::new(server_stdout);
            let mut writer = std::io::BufWriter::new(client_stdin);
            let mut buf = [0u8; 4096];
            loop {
                let n = reader.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                writer.write_all(&buf[..n])?;
                writer.flush()?;
            }
            Ok(())
        });

        // Collect stderr asynchronously
        let client_stderr_handle = client.stderr.take();
        let server_stderr_handle = server.stderr.take();

        // Wait for both processes to complete
        let client_status = client.wait()?;
        let server_status = server.wait()?;

        // Collect stderr output
        let mut client_stderr_buf = Vec::new();
        let mut server_stderr_buf = Vec::new();
        if let Some(mut stderr) = client_stderr_handle {
            let _ = stderr.read_to_end(&mut client_stderr_buf);
        }
        if let Some(mut stderr) = server_stderr_handle {
            let _ = stderr.read_to_end(&mut server_stderr_buf);
        }

        // Wait for pipe threads (may error when processes exit)
        let _ = c2s.join();
        let _ = s2c.join();

        Ok(TransferResult {
            client_success: client_status.success(),
            server_success: server_status.success(),
            client_stderr: String::from_utf8_lossy(&client_stderr_buf).into_owned(),
            server_stderr: String::from_utf8_lossy(&server_stderr_buf).into_owned(),
        })
    }

    /// Run a pull transfer: oc-rsync server (sender) → upstream rsync client (receiver).
    ///
    /// The oc-rsync runs in server sender mode, upstream rsync receives.
    pub fn pull_transfer(&self, source: &Path, dest: &Path) -> io::Result<TransferResult> {
        use std::io::{Read, Write};
        use std::process::Stdio;
        use std::thread;

        // Spawn oc-rsync in --server --sender mode
        let mut server = Command::new(&self.oc_rsync_binary)
            .arg("--server")
            .arg("--sender")
            .arg("-vlogDtprze.iLsfxCIvu")
            .arg(".")
            .arg(source.to_string_lossy().as_ref())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        // Spawn upstream rsync in --server mode (receiver)
        let mut client = Command::new(&self.upstream_binary)
            .arg("--server")
            .arg("-vlogDtprze.iLsfxCIvu")
            .arg(".")
            .arg(dest.to_string_lossy().as_ref())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        // Wire up bidirectional pipes
        let client_stdout = client.stdout.take().unwrap();
        let client_stdin = client.stdin.take().unwrap();
        let server_stdout = server.stdout.take().unwrap();
        let server_stdin = server.stdin.take().unwrap();

        let c2s = thread::spawn(move || -> io::Result<()> {
            let mut reader = std::io::BufReader::new(client_stdout);
            let mut writer = std::io::BufWriter::new(server_stdin);
            let mut buf = [0u8; 4096];
            loop {
                let n = reader.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                writer.write_all(&buf[..n])?;
                writer.flush()?;
            }
            Ok(())
        });

        let s2c = thread::spawn(move || -> io::Result<()> {
            let mut reader = std::io::BufReader::new(server_stdout);
            let mut writer = std::io::BufWriter::new(client_stdin);
            let mut buf = [0u8; 4096];
            loop {
                let n = reader.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                writer.write_all(&buf[..n])?;
                writer.flush()?;
            }
            Ok(())
        });

        let client_stderr_handle = client.stderr.take();
        let server_stderr_handle = server.stderr.take();

        let client_status = client.wait()?;
        let server_status = server.wait()?;

        let mut client_stderr_buf = Vec::new();
        let mut server_stderr_buf = Vec::new();
        if let Some(mut stderr) = client_stderr_handle {
            let _ = stderr.read_to_end(&mut client_stderr_buf);
        }
        if let Some(mut stderr) = server_stderr_handle {
            let _ = stderr.read_to_end(&mut server_stderr_buf);
        }

        let _ = c2s.join();
        let _ = s2c.join();

        Ok(TransferResult {
            client_success: client_status.success(),
            server_success: server_status.success(),
            client_stderr: String::from_utf8_lossy(&client_stderr_buf).into_owned(),
            server_stderr: String::from_utf8_lossy(&server_stderr_buf).into_owned(),
        })
    }
}

/// Result of a server-mode transfer test.
#[derive(Debug)]
pub struct TransferResult {
    /// Whether the client process exited successfully.
    pub client_success: bool,
    /// Whether the server process exited successfully.
    pub server_success: bool,
    /// Stderr output from the client.
    pub client_stderr: String,
    /// Stderr output from the server.
    pub server_stderr: String,
}

impl TransferResult {
    /// Check if both client and server succeeded.
    pub fn success(&self) -> bool {
        self.client_success && self.server_success
    }

    /// Assert that the transfer succeeded, printing debug info on failure.
    pub fn assert_success(&self) {
        if !self.success() {
            eprintln!("=== Transfer failed ===");
            eprintln!("Client success: {}", self.client_success);
            eprintln!("Server success: {}", self.server_success);
            eprintln!("=== Client stderr ===");
            eprintln!("{}", self.client_stderr);
            eprintln!("=== Server stderr ===");
            eprintln!("{}", self.server_stderr);
            panic!("Transfer should have succeeded");
        }
    }
}

/// Locate an upstream rsync binary by version.
pub fn upstream_rsync_binary(version: &str) -> Option<PathBuf> {
    let path = PathBuf::from(format!(
        "target/interop/upstream-install/{version}/bin/rsync"
    ));
    if path.is_file() { Some(path) } else { None }
}
