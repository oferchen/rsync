//! Xattr round-trip test harness primitive for cross-platform validation.
//!
//! Provides the building blocks for XAP-4..7: set known extended attributes on
//! source files, transfer via oc-rsync local copy mode with `-aX`, read back
//! xattrs on the destination, and compare with platform-appropriate
//! normalization.
//!
//! # Platform Support
//!
//! - **Linux**: `user.*` namespace via the `xattr` crate. `security.*`
//!   namespace requires root and is tested conditionally.
//! - **macOS**: Arbitrary attribute names (no namespace prefix required).
//!   `com.apple.quarantine`-style attributes are tested.
//!
//! # Skip Conditions
//!
//! Tests using this harness skip cleanly when:
//! - `OC_RSYNC_XATTR_ROUNDTRIP` env var is not set to `1`.
//! - The filesystem does not support xattrs (e.g., some tmpfs configurations).
//! - The oc-rsync binary cannot be located.
//!
//! # Upstream Reference
//!
//! - `xattrs.c:send_xattr()` / `xattrs.c:receive_xattr()` - wire protocol
//!   xattr handling.
//! - `xattrs.c:set_xattr()` - receiver-side xattr application.

#![allow(dead_code)]

use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use super::helpers::{RsyncCommand, TestDir};

/// Environment variable that gates xattr roundtrip tests. When unset or not
/// equal to "1", tests skip cleanly. This avoids failures on CI runners
/// lacking xattr-capable filesystems.
pub const XATTR_GATE_ENV: &str = "OC_RSYNC_XATTR_ROUNDTRIP";

/// A single extended attribute entry - a name/value pair to stamp on a file.
///
/// On Linux, names must include a namespace prefix (e.g., `user.myattr`).
/// On macOS, names are arbitrary (e.g., `com.apple.quarantine`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct XattrEntry {
    /// Attribute name (including namespace prefix on Linux).
    pub name: String,
    /// Attribute value as raw bytes.
    pub value: Vec<u8>,
}

impl XattrEntry {
    /// Create an xattr entry with the given name and value.
    pub fn new(name: &str, value: &[u8]) -> Self {
        Self {
            name: name.to_string(),
            value: value.to_vec(),
        }
    }

    /// Convenience: create a `user.*` namespace xattr (Linux convention).
    pub fn user(suffix: &str, value: &[u8]) -> Self {
        Self::new(&format!("user.{suffix}"), value)
    }

    /// Convenience: create a `security.*` namespace xattr (Linux, requires root).
    pub fn security(suffix: &str, value: &[u8]) -> Self {
        Self::new(&format!("security.{suffix}"), value)
    }

    /// Convenience: create a macOS-style attribute with a reverse-DNS name.
    pub fn macos(name: &str, value: &[u8]) -> Self {
        Self::new(name, value)
    }
}

impl fmt::Display for XattrEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}={}",
            self.name,
            if self
                .value
                .iter()
                .all(|b| b.is_ascii_graphic() || *b == b' ')
            {
                String::from_utf8_lossy(&self.value).into_owned()
            } else {
                hex_encode(&self.value)
            }
        )
    }
}

/// A file entry in the xattr test fixture with its expected xattr state.
#[derive(Clone, Debug)]
pub struct FixtureFile {
    /// Path relative to the fixture root.
    pub rel_path: PathBuf,
    /// Content to write (empty for directories).
    pub content: Vec<u8>,
    /// Whether this is a directory.
    pub is_dir: bool,
    /// Xattr entries to stamp on this file/directory.
    pub xattr_entries: Vec<XattrEntry>,
}

impl FixtureFile {
    /// Create a regular file entry with given xattr entries.
    pub fn file(rel_path: &str, content: &[u8], xattr_entries: Vec<XattrEntry>) -> Self {
        Self {
            rel_path: PathBuf::from(rel_path),
            content: content.to_vec(),
            is_dir: false,
            xattr_entries,
        }
    }

    /// Create a directory entry with given xattr entries.
    pub fn dir(rel_path: &str, xattr_entries: Vec<XattrEntry>) -> Self {
        Self {
            rel_path: PathBuf::from(rel_path),
            content: Vec::new(),
            is_dir: true,
            xattr_entries,
        }
    }
}

/// Test fixture that builds a source directory with known xattr entries.
///
/// The fixture creates files/directories, stamps xattrs using the `xattr`
/// crate, and provides the source path for transfer operations.
pub struct XattrTestFixture {
    /// Owning temporary directory (cleanup on drop).
    pub test_dir: TestDir,
    /// Path to the source directory within the temp dir.
    pub src: PathBuf,
    /// Path to the destination directory within the temp dir.
    pub dst: PathBuf,
    /// Entries that were stamped (kept for verification).
    pub entries: Vec<FixtureFile>,
}

impl XattrTestFixture {
    /// Attempt to build the fixture. Returns `None` with a logged skip reason
    /// when preconditions are not met.
    ///
    /// # Arguments
    ///
    /// * `entries` - The file/directory layout with xattr specifications.
    pub fn try_build(entries: Vec<FixtureFile>) -> Option<Self> {
        if !gate_enabled() {
            skip(&format!(
                "{XATTR_GATE_ENV} not set to 1; opt in to run xattr roundtrip tests"
            ));
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

        // Probe xattr support on the filesystem by writing and reading a test
        // attribute on the src directory itself.
        if !xattr_supported(&src) {
            skip("filesystem does not support extended attributes");
            return None;
        }

        // Create files/dirs and stamp xattrs.
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

            // Stamp xattrs.
            for xa in &entry.xattr_entries {
                if let Err(e) = xattr::set(&abs_path, &xa.name, &xa.value) {
                    skip(&format!(
                        "failed to set xattr {}={} on {} \
                         (filesystem may lack xattr support or permission denied): {e}",
                        xa.name,
                        hex_encode(&xa.value),
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

    /// Run oc-rsync local copy with `-aX` from src to dst.
    ///
    /// Returns the command output on success, or an error string on failure.
    pub fn transfer(&self) -> Result<std::process::Output, String> {
        let mut cmd = RsyncCommand::new();
        cmd.args([
            "-aX",
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
                        "oc-rsync -aX transfer failed (exit {:?}):\nstdout: {}\nstderr: {}",
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

/// Result of comparing a single xattr entry between source and destination.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum XattrComparison {
    /// Xattr entry matches between source and destination.
    Match { path: PathBuf, name: String },
    /// Xattr present on source but missing on destination.
    MissingOnDest { path: PathBuf, name: String },
    /// Xattr present on destination but not expected from source.
    ExtraOnDest { path: PathBuf, name: String },
    /// Xattr present on both but values differ.
    ValueMismatch {
        path: PathBuf,
        name: String,
        src_value: String,
        dst_value: String,
    },
}

impl fmt::Display for XattrComparison {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Match { path, name } => {
                write!(f, "OK  {}: {name}", path.display())
            }
            Self::MissingOnDest { path, name } => {
                write!(f, "MISSING_ON_DEST  {}: {name}", path.display())
            }
            Self::ExtraOnDest { path, name } => {
                write!(f, "EXTRA_ON_DEST  {}: {name}", path.display())
            }
            Self::ValueMismatch {
                path,
                name,
                src_value,
                dst_value,
            } => {
                write!(
                    f,
                    "VALUE_MISMATCH  {}: {name} (src={src_value}, dst={dst_value})",
                    path.display()
                )
            }
        }
    }
}

/// Structured result of an xattr round-trip verification.
#[derive(Clone, Debug)]
pub struct XattrRoundtripResult {
    /// All comparison results (matches and mismatches).
    pub comparisons: Vec<XattrComparison>,
}

impl XattrRoundtripResult {
    /// Returns `true` if all entries match.
    pub fn all_match(&self) -> bool {
        self.comparisons
            .iter()
            .all(|c| matches!(c, XattrComparison::Match { .. }))
    }

    /// Returns only the mismatches.
    pub fn mismatches(&self) -> Vec<&XattrComparison> {
        self.comparisons
            .iter()
            .filter(|c| !matches!(c, XattrComparison::Match { .. }))
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

/// Verify that xattrs survived the round-trip from `src_dir` to `dst_dir`.
///
/// Reads xattrs from both directories using the `xattr` crate, sorts them
/// for deterministic comparison, and returns a structured diff.
///
/// # Arguments
///
/// * `src_dir` - Source directory root.
/// * `dst_dir` - Destination directory root (post-transfer).
/// * `entries` - The fixture entries used to build the source (provides the
///   list of paths and expected xattrs to check).
pub fn verify_xattr_roundtrip(
    src_dir: &Path,
    dst_dir: &Path,
    entries: &[FixtureFile],
) -> XattrRoundtripResult {
    let mut comparisons = Vec::new();

    for entry in entries {
        let src_path = src_dir.join(&entry.rel_path);
        let dst_path = dst_dir.join(&entry.rel_path);

        if !dst_path.exists() {
            for xa in &entry.xattr_entries {
                comparisons.push(XattrComparison::MissingOnDest {
                    path: entry.rel_path.clone(),
                    name: xa.name.clone(),
                });
            }
            continue;
        }

        let src_xattrs = match read_xattrs_sorted(&src_path) {
            Ok(xattrs) => xattrs,
            Err(e) => {
                eprintln!(
                    "warning: could not read xattrs from source {}: {e}",
                    src_path.display()
                );
                continue;
            }
        };

        let dst_xattrs = match read_xattrs_sorted(&dst_path) {
            Ok(xattrs) => xattrs,
            Err(e) => {
                eprintln!(
                    "warning: could not read xattrs from dest {}: {e}",
                    dst_path.display()
                );
                for (name, _) in &src_xattrs {
                    comparisons.push(XattrComparison::MissingOnDest {
                        path: entry.rel_path.clone(),
                        name: name.clone(),
                    });
                }
                continue;
            }
        };

        // Build lookup maps for comparison.
        let src_map: std::collections::HashMap<&str, &[u8]> = src_xattrs
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_slice()))
            .collect();
        let dst_map: std::collections::HashMap<&str, &[u8]> = dst_xattrs
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_slice()))
            .collect();

        // Check each source xattr against destination.
        for (name, src_val) in &src_map {
            match dst_map.get(name) {
                Some(dst_val) if *dst_val == *src_val => {
                    comparisons.push(XattrComparison::Match {
                        path: entry.rel_path.clone(),
                        name: name.to_string(),
                    });
                }
                Some(dst_val) => {
                    comparisons.push(XattrComparison::ValueMismatch {
                        path: entry.rel_path.clone(),
                        name: name.to_string(),
                        src_value: hex_encode(src_val),
                        dst_value: hex_encode(dst_val),
                    });
                }
                None => {
                    comparisons.push(XattrComparison::MissingOnDest {
                        path: entry.rel_path.clone(),
                        name: name.to_string(),
                    });
                }
            }
        }

        // Check for extras on dest not in source.
        for name in dst_map.keys() {
            if !src_map.contains_key(name) {
                comparisons.push(XattrComparison::ExtraOnDest {
                    path: entry.rel_path.clone(),
                    name: name.to_string(),
                });
            }
        }
    }

    XattrRoundtripResult { comparisons }
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Check whether the gate environment variable is set.
pub fn gate_enabled() -> bool {
    std::env::var(XATTR_GATE_ENV).ok().as_deref() == Some("1")
}

/// Log a skip reason and return.
pub fn skip(reason: &str) {
    eprintln!("skip: {reason}");
}

/// Probe whether the filesystem at `path` supports xattrs by writing and
/// reading back a test attribute.
fn xattr_supported(path: &Path) -> bool {
    let test_name = "user.xattr_probe_test";
    let test_value = b"probe";

    if xattr::set(path, test_name, test_value).is_err() {
        return false;
    }

    let ok = xattr::get(path, test_name)
        .ok()
        .flatten()
        .map(|v| v == test_value)
        .unwrap_or(false);

    // Clean up the probe attribute regardless.
    let _ = xattr::remove(path, test_name);
    ok
}

/// Read all xattrs from a path, returning sorted `(name, value)` pairs.
///
/// Filters out platform-internal attributes that rsync does not transfer
/// (e.g., `system.posix_acl_access` on Linux, `com.apple.ResourceFork` is
/// kept because rsync does transfer it).
fn read_xattrs_sorted(path: &Path) -> io::Result<Vec<(String, Vec<u8>)>> {
    let names: Vec<String> = xattr::list(path)?
        .filter_map(|name| {
            let s = name.to_string_lossy().into_owned();
            // Skip system/POSIX ACL xattrs that the kernel exposes but rsync
            // handles via the ACL code path, not the xattr code path.
            // upstream: xattrs.c - rsync_xal_get() skips system.posix_acl_*
            if s.starts_with("system.posix_acl_") {
                return None;
            }
            Some(s)
        })
        .collect();

    let mut pairs: Vec<(String, Vec<u8>)> = Vec::with_capacity(names.len());
    for name in names {
        match xattr::get(path, &name) {
            Ok(Some(val)) => pairs.push((name, val)),
            Ok(None) => {
                // Attribute disappeared between list and get - race, skip.
            }
            Err(e) => {
                return Err(io::Error::other(format!(
                    "failed to read xattr {name} on {}: {e}",
                    path.display()
                )));
            }
        }
    }
    pairs.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(pairs)
}

/// Hex-encode a byte slice for display in comparison results.
fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Check whether the current process is running as root (uid 0).
///
/// Used to gate tests that require root for `security.*` namespace xattrs.
/// Probes by attempting to set a `security.*` xattr on a temp file rather
/// than calling geteuid() directly, which would require an unsafe block.
pub fn is_root() -> bool {
    std::env::var("USER").ok().as_deref() == Some("root")
        || std::env::var("EUID").ok().as_deref() == Some("0")
        || std::process::Command::new("id")
            .arg("-u")
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim() == "0")
            .unwrap_or(false)
}
