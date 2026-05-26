//! ACL round-trip test harness primitive for cross-platform validation.
//!
//! Provides the building blocks for XAP-4..7: set known ACL entries on source
//! files, transfer via oc-rsync local copy mode with `-A`, read back ACLs on
//! the destination, and compare with platform-appropriate normalization.
//!
//! # Platform Support
//!
//! - **Linux**: POSIX ACLs via `setfacl`/`getfacl` (access + default).
//! - **macOS**: NFSv4-style extended ACLs via `chmod +a`/`ls -le`.
//! - **Windows**: DACL via `icacls` (read-back only; setup uses `icacls /grant`).
//!
//! # Skip Conditions
//!
//! Tests using this harness skip cleanly when:
//! - The filesystem does not support ACLs (e.g., tmpfs without `acl` mount option).
//! - Required platform tools are missing (`setfacl`/`getfacl` on Linux, etc.).
//! - The oc-rsync binary cannot be located.
//!
//! # Upstream Reference
//!
//! - `acls.c:send_acl()` / `acls.c:receive_acl()` - wire protocol ACL handling.
//! - `acls.c:set_acl()` - receiver-side ACL application.

#![allow(dead_code)]

use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::helpers::{RsyncCommand, TestDir};

/// Environment variable that gates ACL roundtrip tests. When unset or not
/// equal to "1", tests skip cleanly. This avoids failures on CI runners
/// lacking ACL-capable filesystems or elevated permissions.
pub const ACL_GATE_ENV: &str = "OC_RSYNC_ACL_ROUNDTRIP";

/// A single ACL entry specification, platform-agnostic.
///
/// Each variant carries the information needed to stamp an ACL entry on
/// a file using the platform-specific tool.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AclEntry {
    /// Named user with read/write/execute permission bits (rwx = 7, r-x = 5, etc.).
    User {
        name: String,
        perms: u8,
    },
    /// Named group with permission bits.
    Group {
        name: String,
        perms: u8,
    },
    /// Default (inheritable) ACL for a named user on a directory (Linux only).
    DefaultUser {
        name: String,
        perms: u8,
    },
    /// Default (inheritable) ACL for a named group on a directory (Linux only).
    DefaultGroup {
        name: String,
        perms: u8,
    },
}

impl AclEntry {
    /// Convenience: named user with full rwx.
    pub fn user_rwx(name: &str) -> Self {
        Self::User {
            name: name.to_string(),
            perms: 7,
        }
    }

    /// Convenience: named user with read+execute.
    pub fn user_rx(name: &str) -> Self {
        Self::User {
            name: name.to_string(),
            perms: 5,
        }
    }

    /// Convenience: named user with read-only.
    pub fn user_r(name: &str) -> Self {
        Self::User {
            name: name.to_string(),
            perms: 4,
        }
    }

    /// Convenience: named group with read+execute.
    pub fn group_rx(name: &str) -> Self {
        Self::Group {
            name: name.to_string(),
            perms: 5,
        }
    }

    /// Convenience: default user entry (directory inheritance).
    pub fn default_user_rwx(name: &str) -> Self {
        Self::DefaultUser {
            name: name.to_string(),
            perms: 7,
        }
    }

    /// Convenience: default group entry (directory inheritance).
    pub fn default_group_rx(name: &str) -> Self {
        Self::DefaultGroup {
            name: name.to_string(),
            perms: 5,
        }
    }
}

/// A file entry in the ACL test fixture with its expected ACL state.
#[derive(Clone, Debug)]
pub struct FixtureFile {
    /// Path relative to the fixture root.
    pub rel_path: PathBuf,
    /// Content to write (empty for directories).
    pub content: Vec<u8>,
    /// Whether this is a directory.
    pub is_dir: bool,
    /// ACL entries to stamp on this file/directory.
    pub acl_entries: Vec<AclEntry>,
}

impl FixtureFile {
    /// Create a regular file entry with given ACL entries.
    pub fn file(rel_path: &str, content: &[u8], acl_entries: Vec<AclEntry>) -> Self {
        Self {
            rel_path: PathBuf::from(rel_path),
            content: content.to_vec(),
            is_dir: false,
            acl_entries,
        }
    }

    /// Create a directory entry with given ACL entries.
    pub fn dir(rel_path: &str, acl_entries: Vec<AclEntry>) -> Self {
        Self {
            rel_path: PathBuf::from(rel_path),
            content: Vec::new(),
            is_dir: true,
            acl_entries,
        }
    }
}

/// Test fixture that builds a source directory with known ACL entries.
///
/// The fixture creates files/directories, stamps ACLs using platform tools,
/// and provides the source path for transfer operations.
pub struct AclTestFixture {
    /// Owning temporary directory (cleanup on drop).
    pub test_dir: TestDir,
    /// Path to the source directory within the temp dir.
    pub src: PathBuf,
    /// Path to the destination directory within the temp dir.
    pub dst: PathBuf,
    /// Entries that were stamped (kept for verification).
    pub entries: Vec<FixtureFile>,
}

impl AclTestFixture {
    /// Attempt to build the fixture. Returns `None` with a logged skip reason
    /// when preconditions are not met.
    ///
    /// # Arguments
    ///
    /// * `entries` - The file/directory layout with ACL specifications.
    pub fn try_build(entries: Vec<FixtureFile>) -> Option<Self> {
        if !gate_enabled() {
            skip(&format!(
                "{ACL_GATE_ENV} not set to 1; opt in to run ACL roundtrip tests"
            ));
            return None;
        }

        if !platform_acl_tools_available() {
            skip("required ACL platform tools not available");
            return None;
        }

        let test_dir = match TestDir::new() {
            Ok(d) => d,
            Err(e) => {
                skip(&format!("failed to create test dir: {e}"));
                return None;
            }
        };

        let src = match test_dir.mkdir("src") {
            Ok(p) => p,
            Err(e) => {
                skip(&format!("failed to create src dir: {e}"));
                return None;
            }
        };

        let dst = match test_dir.mkdir("dst") {
            Ok(p) => p,
            Err(e) => {
                skip(&format!("failed to create dst dir: {e}"));
                return None;
            }
        };

        // Create files/dirs and stamp ACLs.
        for entry in &entries {
            let abs_path = src.join(&entry.rel_path);
            if entry.is_dir {
                if let Err(e) = fs::create_dir_all(&abs_path) {
                    skip(&format!(
                        "failed to create dir {}: {e}",
                        entry.rel_path.display()
                    ));
                    return None;
                }
            } else {
                if let Some(parent) = abs_path.parent() {
                    if let Err(e) = fs::create_dir_all(parent) {
                        skip(&format!("failed to create parent dir: {e}"));
                        return None;
                    }
                }
                if let Err(e) = fs::write(&abs_path, &entry.content) {
                    skip(&format!(
                        "failed to write {}: {e}",
                        entry.rel_path.display()
                    ));
                    return None;
                }
            }

            // Stamp ACLs.
            for acl in &entry.acl_entries {
                if let Err(e) = stamp_acl(&abs_path, acl) {
                    skip(&format!(
                        "failed to stamp ACL on {} (filesystem may lack ACL support): {e}",
                        entry.rel_path.display()
                    ));
                    return None;
                }
            }
        }

        Some(Self {
            test_dir,
            src,
            dst,
            entries,
        })
    }

    /// Run oc-rsync local copy with `-aA` from src to dst.
    ///
    /// Returns the command output on success, or an error string on failure.
    pub fn transfer(&self) -> Result<std::process::Output, String> {
        let mut cmd = RsyncCommand::new();
        cmd.args([
            "-aA",
            "--numeric-ids",
            &format!("{}/", self.src.display()),
            &self.dst.to_string_lossy(),
        ]);
        match cmd.run() {
            Ok(output) => {
                if output.status.success() {
                    Ok(output)
                } else {
                    Err(format!(
                        "oc-rsync -aA transfer failed (exit {:?}):\nstdout: {}\nstderr: {}",
                        output.status.code(),
                        String::from_utf8_lossy(&output.stdout),
                        String::from_utf8_lossy(&output.stderr),
                    ))
                }
            }
            Err(e) => Err(format!("failed to spawn oc-rsync: {e}")),
        }
    }
}

/// Result of comparing a single ACL entry between source and destination.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AclComparison {
    /// ACL entry matches between source and destination.
    Match {
        path: PathBuf,
        entry: String,
    },
    /// ACL entry present on source but missing on destination.
    MissingOnDest {
        path: PathBuf,
        entry: String,
    },
    /// ACL entry present on destination but not expected from source.
    ExtraOnDest {
        path: PathBuf,
        entry: String,
    },
    /// ACL entry present on both but permissions differ.
    PermsMismatch {
        path: PathBuf,
        entry: String,
        src_perms: String,
        dst_perms: String,
    },
}

impl fmt::Display for AclComparison {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Match { path, entry } => {
                write!(f, "OK  {}: {entry}", path.display())
            }
            Self::MissingOnDest { path, entry } => {
                write!(f, "MISSING_ON_DEST  {}: {entry}", path.display())
            }
            Self::ExtraOnDest { path, entry } => {
                write!(f, "EXTRA_ON_DEST  {}: {entry}", path.display())
            }
            Self::PermsMismatch {
                path,
                entry,
                src_perms,
                dst_perms,
            } => {
                write!(
                    f,
                    "PERMS_MISMATCH  {}: {entry} (src={src_perms}, dst={dst_perms})",
                    path.display()
                )
            }
        }
    }
}

/// Structured result of an ACL round-trip verification.
#[derive(Clone, Debug)]
pub struct AclRoundtripResult {
    /// All comparison results (matches and mismatches).
    pub comparisons: Vec<AclComparison>,
}

impl AclRoundtripResult {
    /// Returns `true` if all entries match.
    pub fn all_match(&self) -> bool {
        self.comparisons
            .iter()
            .all(|c| matches!(c, AclComparison::Match { .. }))
    }

    /// Returns only the mismatches.
    pub fn mismatches(&self) -> Vec<&AclComparison> {
        self.comparisons
            .iter()
            .filter(|c| !matches!(c, AclComparison::Match { .. }))
            .collect()
    }

    /// Formats mismatches as a human-readable report string.
    pub fn mismatch_report(&self) -> String {
        let mismatches = self.mismatches();
        if mismatches.is_empty() {
            return String::from("(no mismatches)");
        }
        mismatches
            .iter()
            .map(|m| m.to_string())
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// Verify that ACLs survived the round-trip from `src_dir` to `dst_dir`.
///
/// Reads ACLs from both directories using platform tools, normalizes them
/// (sort order, whitespace, platform-specific canonicalization), and returns
/// a structured diff.
///
/// # Arguments
///
/// * `src_dir` - Source directory root.
/// * `dst_dir` - Destination directory root (post-transfer).
/// * `entries` - The fixture entries used to build the source (provides the
///   list of paths to check).
pub fn verify_acl_roundtrip(
    src_dir: &Path,
    dst_dir: &Path,
    entries: &[FixtureFile],
) -> AclRoundtripResult {
    let mut comparisons = Vec::new();

    for entry in entries {
        let src_path = src_dir.join(&entry.rel_path);
        let dst_path = dst_dir.join(&entry.rel_path);

        if !dst_path.exists() {
            for acl in &entry.acl_entries {
                comparisons.push(AclComparison::MissingOnDest {
                    path: entry.rel_path.clone(),
                    entry: format_acl_entry(acl),
                });
            }
            continue;
        }

        let src_acl_lines = match read_acl_normalized(&src_path) {
            Ok(lines) => lines,
            Err(e) => {
                eprintln!(
                    "warning: could not read ACL from source {}: {e}",
                    src_path.display()
                );
                continue;
            }
        };

        let dst_acl_lines = match read_acl_normalized(&dst_path) {
            Ok(lines) => lines,
            Err(e) => {
                eprintln!(
                    "warning: could not read ACL from dest {}: {e}",
                    dst_path.display()
                );
                // All source entries are missing.
                for line in &src_acl_lines {
                    comparisons.push(AclComparison::MissingOnDest {
                        path: entry.rel_path.clone(),
                        entry: line.clone(),
                    });
                }
                continue;
            }
        };

        // Compare normalized ACL lines.
        for line in &src_acl_lines {
            if dst_acl_lines.contains(line) {
                comparisons.push(AclComparison::Match {
                    path: entry.rel_path.clone(),
                    entry: line.clone(),
                });
            } else {
                // Check if it is a perms mismatch (same user/group, different perms).
                let prefix = acl_line_prefix(line);
                let dst_match = dst_acl_lines.iter().find(|dl| acl_line_prefix(dl) == prefix);
                if let Some(dst_line) = dst_match {
                    comparisons.push(AclComparison::PermsMismatch {
                        path: entry.rel_path.clone(),
                        entry: prefix,
                        src_perms: acl_line_perms(line),
                        dst_perms: acl_line_perms(dst_line),
                    });
                } else {
                    comparisons.push(AclComparison::MissingOnDest {
                        path: entry.rel_path.clone(),
                        entry: line.clone(),
                    });
                }
            }
        }

        // Check for extras on dest not in source.
        for line in &dst_acl_lines {
            if !src_acl_lines.contains(line) {
                let prefix = acl_line_prefix(line);
                let in_src = src_acl_lines.iter().any(|sl| acl_line_prefix(sl) == prefix);
                if !in_src {
                    comparisons.push(AclComparison::ExtraOnDest {
                        path: entry.rel_path.clone(),
                        entry: line.clone(),
                    });
                }
            }
        }
    }

    AclRoundtripResult { comparisons }
}

// ---------------------------------------------------------------------------
// Platform-specific ACL operations
// ---------------------------------------------------------------------------

/// Check whether the gate environment variable is set.
pub fn gate_enabled() -> bool {
    std::env::var(ACL_GATE_ENV).ok().as_deref() == Some("1")
}

/// Log a skip reason and return.
pub fn skip(reason: &str) {
    eprintln!("skip: {reason}");
}

/// Check whether the platform-specific ACL tools are available.
#[cfg(target_os = "linux")]
fn platform_acl_tools_available() -> bool {
    command_exists("setfacl") && command_exists("getfacl")
}

#[cfg(target_os = "macos")]
fn platform_acl_tools_available() -> bool {
    // macOS ships chmod and ls in base; they support +a and -le.
    command_exists("chmod") && command_exists("ls")
}

#[cfg(target_os = "windows")]
fn platform_acl_tools_available() -> bool {
    command_exists("icacls")
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn platform_acl_tools_available() -> bool {
    false
}

/// Check if a command is available on PATH.
fn command_exists(cmd: &str) -> bool {
    #[cfg(unix)]
    {
        Command::new("sh")
            .arg("-c")
            .arg(format!("command -v {cmd} >/dev/null 2>&1"))
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }
    #[cfg(windows)]
    {
        Command::new("where")
            .arg(cmd)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = cmd;
        false
    }
}

/// Stamp an ACL entry on a path using platform-specific tools.
#[cfg(target_os = "linux")]
fn stamp_acl(path: &Path, entry: &AclEntry) -> io::Result<()> {
    let (spec, default) = match entry {
        AclEntry::User { name, perms } => (format!("u:{name}:{}", perms_to_rwx(*perms)), false),
        AclEntry::Group { name, perms } => (format!("g:{name}:{}", perms_to_rwx(*perms)), false),
        AclEntry::DefaultUser { name, perms } => {
            (format!("u:{name}:{}", perms_to_rwx(*perms)), true)
        }
        AclEntry::DefaultGroup { name, perms } => {
            (format!("g:{name}:{}", perms_to_rwx(*perms)), true)
        }
    };

    let mut cmd = Command::new("setfacl");
    if default {
        cmd.arg("-d");
    }
    cmd.arg("-m").arg(&spec).arg(path);
    let output = cmd.output()?;
    if !output.status.success() {
        return Err(io::Error::other(format!(
            "setfacl {} {} on {} failed: {}",
            if default { "-d -m" } else { "-m" },
            spec,
            path.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn stamp_acl(path: &Path, entry: &AclEntry) -> io::Result<()> {
    // macOS uses `chmod +a` with NFSv4-style ACL syntax.
    // Default ACLs do not exist on macOS; map them to file_inherit/directory_inherit.
    let (ace_str, _is_default) = match entry {
        AclEntry::User { name, perms } => (
            format!(
                "{name} allow {}",
                macos_perms_str(*perms, path.is_dir())
            ),
            false,
        ),
        AclEntry::Group { name, perms } => (
            format!(
                "group:{name} allow {}",
                macos_perms_str(*perms, path.is_dir())
            ),
            false,
        ),
        AclEntry::DefaultUser { name, perms } => (
            format!(
                "{name} allow {},file_inherit,directory_inherit",
                macos_perms_str(*perms, true)
            ),
            true,
        ),
        AclEntry::DefaultGroup { name, perms } => (
            format!(
                "group:{name} allow {},file_inherit,directory_inherit",
                macos_perms_str(*perms, true)
            ),
            true,
        ),
    };

    let output = Command::new("chmod")
        .arg("+a")
        .arg(&ace_str)
        .arg(path)
        .output()?;
    if !output.status.success() {
        return Err(io::Error::other(format!(
            "chmod +a '{}' on {} failed: {}",
            ace_str,
            path.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(())
}

#[cfg(target_os = "windows")]
fn stamp_acl(path: &Path, entry: &AclEntry) -> io::Result<()> {
    let (principal, perms, inheritance) = match entry {
        AclEntry::User { name, perms } => (name.clone(), *perms, false),
        AclEntry::Group { name, perms } => (name.clone(), *perms, false),
        AclEntry::DefaultUser { name, perms } => (name.clone(), *perms, true),
        AclEntry::DefaultGroup { name, perms } => (name.clone(), *perms, true),
    };

    let perm_str = windows_perms_str(perms);
    let mut cmd = Command::new("icacls");
    cmd.arg(path);
    if inheritance {
        cmd.arg("/grant").arg(format!("{principal}:(OI)(CI){perm_str}"));
    } else {
        cmd.arg("/grant").arg(format!("{principal}:{perm_str}"));
    }
    let output = cmd.output()?;
    if !output.status.success() {
        return Err(io::Error::other(format!(
            "icacls /grant on {} failed: {}",
            path.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(())
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn stamp_acl(_path: &Path, _entry: &AclEntry) -> io::Result<()> {
    Err(io::Error::other("ACL stamping not supported on this platform"))
}

/// Read ACL entries from a path and return normalized, sorted lines.
///
/// Each line has the format `<type>:<qualifier>:<perms>` on Linux,
/// or a normalized representation on macOS/Windows. Comment lines,
/// empty lines, and base permission entries (user::, group::, other::)
/// are excluded to focus on extended ACL entries only.
#[cfg(target_os = "linux")]
fn read_acl_normalized(path: &Path) -> io::Result<Vec<String>> {
    let output = Command::new("getfacl")
        .arg("-p")
        .arg("--omit-header")
        .arg("--absolute-names")
        .arg(path)
        .output()?;
    if !output.status.success() {
        return Err(io::Error::other(format!(
            "getfacl on {} failed: {}",
            path.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    let body = String::from_utf8_lossy(&output.stdout);
    let mut lines: Vec<String> = body
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        // Keep only extended entries (named users/groups and defaults).
        // Base entries (user::rwx, group::r-x, other::r-x, mask::rwx) are
        // included because rsync -A preserves them and differences matter.
        .map(String::from)
        .collect();
    lines.sort();
    Ok(lines)
}

#[cfg(target_os = "macos")]
fn read_acl_normalized(path: &Path) -> io::Result<Vec<String>> {
    // `ls -led` shows extended ACLs.  Each ACE is on its own line,
    // numbered.  Format: ` N: <type>:<qualifier> <allow|deny> <perms>`
    let output = Command::new("ls")
        .arg("-led")
        .arg(path)
        .output()?;
    if !output.status.success() {
        return Err(io::Error::other(format!(
            "ls -led on {} failed: {}",
            path.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    let body = String::from_utf8_lossy(&output.stdout);
    let mut lines: Vec<String> = body
        .lines()
        .filter_map(|l| {
            let trimmed = l.trim();
            // ACE lines are indented and start with a number followed by colon.
            if trimmed.starts_with(|c: char| c.is_ascii_digit()) && trimmed.contains(':') {
                // Strip the leading index number for stable comparison.
                let after_colon = trimmed.splitn(2, ": ").nth(1)?;
                Some(normalize_macos_ace(after_colon))
            } else {
                None
            }
        })
        .collect();
    lines.sort();
    Ok(lines)
}

#[cfg(target_os = "windows")]
fn read_acl_normalized(path: &Path) -> io::Result<Vec<String>> {
    let output = Command::new("icacls")
        .arg(path)
        .output()?;
    if !output.status.success() {
        return Err(io::Error::other(format!(
            "icacls on {} failed: {}",
            path.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    let body = String::from_utf8_lossy(&output.stdout);
    // icacls output: one line per ACE, format: `<path> <SID>:<perm>`
    // or continuation lines with leading spaces.
    let mut lines: Vec<String> = body
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && l.contains(':') && !l.starts_with("Successfully"))
        .map(|l| {
            // Strip the path prefix, keep just `SID:(perms)`.
            if let Some(idx) = l.find(' ') {
                l[idx..].trim().to_string()
            } else {
                l.to_string()
            }
        })
        .collect();
    lines.sort();
    Ok(lines)
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn read_acl_normalized(_path: &Path) -> io::Result<Vec<String>> {
    Err(io::Error::other("ACL reading not supported on this platform"))
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Convert a 3-bit permission value to rwx string.
fn perms_to_rwx(perms: u8) -> String {
    let r = if perms & 4 != 0 { 'r' } else { '-' };
    let w = if perms & 2 != 0 { 'w' } else { '-' };
    let x = if perms & 1 != 0 { 'x' } else { '-' };
    format!("{r}{w}{x}")
}

/// Convert perms to macOS ACL permission string.
#[cfg(target_os = "macos")]
fn macos_perms_str(perms: u8, is_dir: bool) -> String {
    let mut parts = Vec::new();
    if perms & 4 != 0 {
        parts.push("read");
    }
    if perms & 2 != 0 {
        parts.push("write");
    }
    if perms & 1 != 0 {
        if is_dir {
            parts.push("execute");
        } else {
            parts.push("execute");
        }
    }
    parts.join(",")
}

/// Normalize a macOS ACE line for stable comparison.
///
/// Strips whitespace variations, lowercases, and sorts permission
/// keywords within each ACE.
#[cfg(target_os = "macos")]
fn normalize_macos_ace(ace: &str) -> String {
    // Typical format: "user:name allow read,write,execute,..."
    // Normalize by lowercasing and sorting the permission list.
    let lower = ace.trim().to_lowercase();
    if let Some((prefix, perms_part)) = lower.rsplit_once(' ') {
        let mut perms: Vec<&str> = perms_part.split(',').map(str::trim).collect();
        perms.sort();
        format!("{prefix} {}", perms.join(","))
    } else {
        lower
    }
}

/// Convert perms to Windows icacls permission string.
#[cfg(target_os = "windows")]
fn windows_perms_str(perms: u8) -> String {
    // Map rwx to icacls simple permissions.
    match perms {
        7 => "(F)".to_string(),  // Full
        6 => "(M)".to_string(),  // Modify (rw)
        5 => "(RX)".to_string(), // Read+Execute
        4 => "(R)".to_string(),  // Read
        _ => "(R)".to_string(),  // Fallback
    }
}

/// Format an AclEntry for display in comparison results.
fn format_acl_entry(entry: &AclEntry) -> String {
    match entry {
        AclEntry::User { name, perms } => format!("user:{name}:{}", perms_to_rwx(*perms)),
        AclEntry::Group { name, perms } => format!("group:{name}:{}", perms_to_rwx(*perms)),
        AclEntry::DefaultUser { name, perms } => {
            format!("default:user:{name}:{}", perms_to_rwx(*perms))
        }
        AclEntry::DefaultGroup { name, perms } => {
            format!("default:group:{name}:{}", perms_to_rwx(*perms))
        }
    }
}

/// Extract the "prefix" of an ACL line for matching (everything before the
/// last `:` on Linux format lines, or the qualifier on macOS/Windows).
fn acl_line_prefix(line: &str) -> String {
    // Linux format: `user:name:rwx` or `default:user:name:rwx`
    // We want `user:name` or `default:user:name` as prefix.
    if let Some(last_colon) = line.rfind(':') {
        line[..last_colon].to_string()
    } else {
        line.to_string()
    }
}

/// Extract the "perms" portion of an ACL line (after the last `:`).
fn acl_line_perms(line: &str) -> String {
    if let Some(last_colon) = line.rfind(':') {
        line[last_colon + 1..].to_string()
    } else {
        line.to_string()
    }
}
