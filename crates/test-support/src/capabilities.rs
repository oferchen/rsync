//! Compile-time capability discovery for the binary under test.
//!
//! Several options are gated behind Cargo features that are off in some CI
//! cells - the musl and `no-incremental-flist` cells build without `acl` and
//! `xattr`, and `oc-rsync -A` there exits 1 at preflight with "POSIX ACLs are
//! not supported on this client". A test that hard-codes such an option is red
//! on those cells for a reason that has nothing to do with the behaviour under
//! test.
//!
//! The fix is neither to delete the option nor to blanket-skip the test: it is
//! to ask the binary what it supports and to record what was consequently not
//! exercised. Upstream's own testsuite does exactly this - `acl-symlink-race`
//! greps `--version` output and calls `test_skipped()` when ACLs are absent
//! (`testsuite/acl-symlink-race_test.py:50`).
//!
//! A skipped row must stay visible. [`Capabilities::skip_reason`] returns the
//! reason so the caller can print it, and callers are expected to assert that
//! the unconditional rows still ran - otherwise a build with every optional
//! feature off would pass the test vacuously.

use std::collections::BTreeSet;
use std::process::Command;
use std::sync::OnceLock;

use crate::bin_path::oc_rsync_bin;

/// Capability names as printed in the `Capabilities:` block of `--version`.
///
/// These are the strings the binary itself emits, not an independent list, so
/// they cannot drift from what the build actually supports.
pub mod names {
    /// POSIX ACL support, gated by the `acl` Cargo feature. Required by `-A`.
    pub const ACLS: &str = "ACLs";
    /// Extended-attribute support, gated by the `xattr` feature. Required by `-X`.
    pub const XATTRS: &str = "xattrs";
    /// Access-time preservation. Required by `-U`.
    pub const ATIMES: &str = "atimes";
    /// Creation-time preservation. Required by `-N`.
    pub const CRTIMES: &str = "crtimes";
}

/// The capability set advertised by the `oc-rsync` binary under test.
///
/// Probed once per process and cached; the binary cannot change underneath a
/// single test run.
#[derive(Debug, Clone)]
pub struct Capabilities {
    advertised: BTreeSet<String>,
}

impl Capabilities {
    /// Returns the cached capability set, probing the binary on first use.
    ///
    /// # Panics
    ///
    /// Panics if `oc-rsync --version` cannot be run or exits non-zero, which
    /// would mean the binary under test is unusable - a condition that must
    /// fail loudly rather than degrade into "no capabilities".
    pub fn probe() -> &'static Self {
        static CAPABILITIES: OnceLock<Capabilities> = OnceLock::new();
        CAPABILITIES.get_or_init(|| {
            let bin = oc_rsync_bin();
            let output = Command::new(&bin)
                .arg("--version")
                .output()
                .unwrap_or_else(|e| panic!("run {} --version: {e}", bin.display()));
            assert!(
                output.status.success(),
                "{} --version exited {}",
                bin.display(),
                output.status
            );
            Self::from_version_output(&String::from_utf8_lossy(&output.stdout))
        })
    }

    /// Parses the `Capabilities:` block out of `--version` output.
    ///
    /// The block is an indented, comma-separated list that wraps across lines
    /// and ends at the next unindented heading, so parsing keys on indentation
    /// rather than on a fixed line count.
    fn from_version_output(stdout: &str) -> Self {
        let mut advertised = BTreeSet::new();
        let mut in_block = false;
        for line in stdout.lines() {
            if line.starts_with("Capabilities:") {
                in_block = true;
                continue;
            }
            if in_block {
                // The block ends at the next unindented line, which is the
                // following heading ("Optimizations:", etc.).
                if !line.starts_with(char::is_whitespace) {
                    break;
                }
                advertised.extend(
                    line.split(',')
                        .map(|item| item.trim().to_owned())
                        .filter(|item| !item.is_empty()),
                );
            }
        }
        assert!(
            !advertised.is_empty(),
            "no Capabilities: block in --version output; the probe would \
             silently report every capability as absent:\n{stdout}"
        );
        Self { advertised }
    }

    /// Reports whether the binary advertises `name`.
    #[must_use]
    pub fn has(&self, name: &str) -> bool {
        self.advertised.contains(name)
    }

    /// Returns a printable reason when any of `required` is missing.
    ///
    /// `None` means every requirement is met and the caller should run the
    /// case. `Some(reason)` is intended to be printed, so a skipped case leaves
    /// a trace in the test log rather than vanishing.
    #[must_use]
    pub fn skip_reason(&self, required: &[&str]) -> Option<String> {
        let missing: Vec<&str> = required
            .iter()
            .copied()
            .filter(|name| !self.has(name))
            .collect();
        if missing.is_empty() {
            return None;
        }
        Some(format!(
            "binary lacks {} (build without the matching Cargo feature)",
            missing.join(", ")
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
oc-rsync v0.6.4 (revision #deadbeef) protocol version 32
Capabilities:
    64-bit files, symlinks, atimes, batchfiles, inplace, append, ACLs,
    xattrs, iconv, crtimes
Optimizations:
    SIMD-roll, jemalloc
";

    #[test]
    fn parses_a_wrapped_capability_block() {
        let caps = Capabilities::from_version_output(SAMPLE);
        for expected in [names::ACLS, names::XATTRS, names::ATIMES, names::CRTIMES] {
            assert!(caps.has(expected), "{expected} should be advertised");
        }
        // Proves the block terminates at the next heading rather than running on.
        assert!(
            !caps.has("SIMD-roll"),
            "parsing must stop at Optimizations:"
        );
    }

    #[test]
    fn absent_capability_yields_a_reason_naming_it() {
        let caps = Capabilities::from_version_output(SAMPLE);
        assert!(caps.skip_reason(&[names::ACLS]).is_none());
        let reason = caps
            .skip_reason(&["no-such-capability"])
            .expect("missing capability must produce a reason");
        assert!(
            reason.contains("no-such-capability"),
            "the reason must name what is missing, got: {reason}"
        );
    }

    #[test]
    fn a_version_without_the_block_is_an_error_not_an_empty_set() {
        // The dangerous failure mode: silently reporting every capability
        // absent, which would skip every gated case and pass vacuously.
        let result = std::panic::catch_unwind(|| {
            Capabilities::from_version_output("oc-rsync v0.6.4\nOptimizations:\n    none\n")
        });
        assert!(
            result.is_err(),
            "a missing block must panic, not yield {{}}"
        );
    }
}
