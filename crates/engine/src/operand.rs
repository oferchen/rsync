//! Transfer-operand classification: the single source of truth for deciding
//! whether an operand names a local path, a remote SSH host, or a daemon module.
//!
//! Every caller - the CLI front end, the `core` remote-invocation planner, and
//! the local-copy executor - routes through [`operand_is_remote`] /
//! [`classify_operand`] here so that one operand can never be classified two
//! different ways by two code paths (a #7153-class defect: a divergent verdict
//! ships the wrong operand to the remote server).
//!
//! # Upstream Reference
//!
//! Mirrors `options.c:check_for_hostspec()` / `parse_hostspec()` in upstream
//! rsync 3.4.4. The canonical rule: scanning left to right, a `:` reached
//! before any `/` marks a host spec (`host:path`); a following `:` marks a
//! daemon module (`host::module`); an `[ipv6]` literal guards the colons of an
//! address; and a `/` (or end of string) reached first means a plain local
//! path. Upstream is a Unix tool with no notion of drive letters, so `C:` is a
//! genuine host spec there.
//!
//! oc-rsync layers two deliberate, documented accommodations on top of that
//! rule so native Windows paths are usable:
//!
//! - A `\` is treated exactly like `/` (a path separator that, seen before any
//!   `:`, means local). This holds on every platform so backslash paths such as
//!   `dir\sub:file` classify as local uniformly.
//! - Windows path prefixes (drive letter `C:`, UNC `\\server`, `\\?\`, `\\.\`)
//!   are local, but only under `#[cfg(windows)]`. On Unix the drive-letter
//!   exemption is off, keeping `C:` upstream-faithful (host `C`). The detector,
//!   [`has_windows_path_prefix`], is platform-neutral and unit-tested on Linux
//!   CI even though the classifier only consults it when built for Windows.

use std::ffi::OsStr;

/// The three ways a transfer operand can resolve, per upstream
/// `check_for_hostspec()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperandKind {
    /// A path on the local filesystem (no host spec).
    Local,
    /// A remote host reached over a remote shell: `host:path`, `user@host:path`,
    /// `[ipv6]:path`, or an `ssh://` URL.
    Remote,
    /// An rsync daemon module: `host::module` or an `rsync://` URL.
    Daemon,
}

impl OperandKind {
    /// Returns `true` when the operand is not local, i.e. its bytes travel to a
    /// remote server. Both [`Remote`](Self::Remote) and [`Daemon`](Self::Daemon)
    /// are remote; only [`Local`](Self::Local) stays on this host.
    #[must_use]
    pub fn is_remote(self) -> bool {
        matches!(self, OperandKind::Remote | OperandKind::Daemon)
    }
}

/// Returns `true` when `bytes` begin with a Windows-native path prefix that must
/// be treated as a local path: a drive letter (`C:`), a UNC share (`\\server`
/// or `//server`), or an extended-length / device prefix (`\\?\`, `\\.\`).
///
/// Pure and platform-neutral: it inspects only the leading ASCII bytes, which
/// [`OsStr::to_string_lossy`] preserves verbatim on every platform (the relevant
/// bytes - separators, `?`, `.`, `:`, ASCII letters - are never lossy). That is
/// what lets the Windows-prefix logic compile and be unit-tested on Linux CI
/// rather than hiding behind `#[cfg(windows)]`.
#[must_use]
pub fn has_windows_path_prefix(bytes: &[u8]) -> bool {
    let is_sep = |b: u8| b == b'/' || b == b'\\';

    // Extended-length / device namespace: \\?\... or \\.\...
    if bytes.len() >= 4
        && is_sep(bytes[0])
        && is_sep(bytes[1])
        && (bytes[2] == b'?' || bytes[2] == b'.')
        && is_sep(bytes[3])
    {
        return true;
    }

    // UNC share: \\server or //server.
    if bytes.len() >= 2 && is_sep(bytes[0]) && is_sep(bytes[1]) {
        return true;
    }

    // Drive letter: C:...
    if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        return true;
    }

    false
}

/// Classifies a transfer operand as [`Local`](OperandKind::Local),
/// [`Remote`](OperandKind::Remote), or [`Daemon`](OperandKind::Daemon).
///
/// This is the single source of truth mirroring upstream
/// `options.c:check_for_hostspec()`; see the [module docs](self) for the exact
/// rule and the oc-rsync Windows accommodations.
#[must_use]
pub fn classify_operand(path: &OsStr) -> OperandKind {
    let text = path.to_string_lossy();
    let bytes = text.as_bytes();

    // Windows-native path prefixes are local - but only when built for Windows.
    // On Unix "C:" is a real host spec (upstream has no drive letters), so this
    // exemption stays off to preserve upstream fidelity. The detector itself is
    // platform-neutral and covered by the Linux CI tests below.
    #[cfg(windows)]
    if has_windows_path_prefix(bytes) {
        return OperandKind::Local;
    }

    // upstream: check_for_hostspec() tests URL_PREFIX ("rsync://") first. The
    // match is case-sensitive so that only lowercase URLs are treated as URLs;
    // an uppercase "RSYNC://" falls through and is classified by host below,
    // matching upstream's real-world behaviour and the existing test corpus.
    if bytes.starts_with(b"rsync://") {
        return OperandKind::Daemon;
    }
    // ssh:// remote-shell URL. Upstream has no ssh:// branch in
    // check_for_hostspec; oc-rsync recognises it as a remote-shell operand.
    if bytes.starts_with(b"ssh://") {
        return OperandKind::Remote;
    }

    classify_hostspec(bytes)
}

/// Scans `bytes` for the first host-terminating character, mirroring upstream
/// `parse_hostspec()` (called with a null `port_ptr`): a `:` before any `/`
/// marks a host, a `/` (or end of input) marks a local path, an `@` resets the
/// host start without terminating, and `[..]` guards an IPv6 literal's colons.
///
/// oc-rsync additionally treats `\` like `/` so Windows-style backslash paths
/// classify as local on every platform.
fn classify_hostspec(bytes: &[u8]) -> OperandKind {
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b':' => {
                // A second colon right after the first is a daemon module spec
                // (`host::module`); upstream: check_for_hostspec's `*path == ':'`.
                return if bytes.get(i + 1) == Some(&b':') {
                    OperandKind::Daemon
                } else {
                    OperandKind::Remote
                };
            }
            // '/' terminates the host with no port context -> local (upstream
            // returns NULL). '\' is oc-rsync's cross-platform path separator.
            b'/' | b'\\' => return OperandKind::Local,
            // 'user@host': '@' resets the host start but is not a terminator.
            b'@' => i += 1,
            // '[ipv6]': the inner ':' are address bytes, not host terminators.
            b'[' => {
                i += 1;
                while i < bytes.len() && bytes[i] != b']' && bytes[i] != b'/' {
                    i += 1;
                }
                if i < bytes.len() && bytes[i] == b']' {
                    i += 1;
                } else {
                    return OperandKind::Local;
                }
            }
            _ => i += 1,
        }
    }
    OperandKind::Local
}

/// Returns `true` when `path` names a remote operand (SSH host or daemon
/// module). Convenience wrapper over [`classify_operand`]; equivalent to
/// `classify_operand(path).is_remote()`.
#[must_use]
pub fn operand_is_remote(path: &OsStr) -> bool {
    classify_operand(path).is_remote()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;

    /// Shared corpus exercised by both the pure prefix detector and the
    /// classifier. Encodes WHY each verdict holds: upstream
    /// `check_for_hostspec()` (a `:` before any `/` is a host; `::` is a daemon
    /// module; `[ipv6]:` guards the address colons) plus oc-rsync's documented
    /// backslash-as-separator rule. This is the #7153-class regression guard: a
    /// single classifier means the CLI, core, and local-copy call sites can
    /// never disagree on one operand.
    ///
    /// Each row is `(input, unix_kind, is_windows_prefix)`. `unix_kind` is the
    /// verdict on a non-Windows target (the CI platform), where the drive-letter
    /// exemption is off and `C:` is a genuine host per upstream.
    const CORPUS: &[(&str, OperandKind, bool)] = &[
        // Windows-shaped inputs: local on Windows via the prefix detector, but
        // upstream-faithful host specs on Unix (drive exemption is Windows-only).
        (r"C:\x", OperandKind::Remote, true),
        ("c:rel", OperandKind::Remote, true),
        // Backslash/forward-slash UNC + device prefixes: local everywhere
        // because a separator precedes any colon (oc-rsync backslash rule) or
        // there is no colon at all.
        (r"\\srv\share", OperandKind::Local, true),
        (r"\\?\C:\x", OperandKind::Local, true),
        (r"\\.\pipe", OperandKind::Local, true),
        ("//srv/share", OperandKind::Local, true),
        // Genuine remotes.
        ("host:path", OperandKind::Remote, false),
        ("user@host:path", OperandKind::Remote, false),
        ("[::1]:path", OperandKind::Remote, false),
        ("rsync://h/m", OperandKind::Daemon, false),
        ("host::module", OperandKind::Daemon, false),
        // Plain local paths.
        ("./local", OperandKind::Local, false),
        ("/abs/path", OperandKind::Local, false),
    ];

    #[test]
    fn windows_prefix_detector_is_ci_testable() {
        // The Windows-prefix logic is exercised on Linux CI directly, proving
        // the drive-letter / UNC / \\?\ / \\.\ detection the #[cfg(windows)]
        // classifier branch depends on without needing a Windows runner.
        for (input, _unix_kind, is_prefix) in CORPUS {
            assert_eq!(
                has_windows_path_prefix(input.as_bytes()),
                *is_prefix,
                "has_windows_path_prefix({input:?})"
            );
        }
    }

    #[test]
    #[cfg(not(windows))]
    fn classifier_matches_upstream_on_unix() {
        // On the CI (Linux) platform the drive-letter exemption is off, so the
        // classifier must match upstream check_for_hostspec verbatim: "C:" is a
        // host, not a drive. This is the exact behaviour core/transfer_role
        // already relied on; unifying the parser/engine impls onto it removes
        // their non-upstream "C: is local on Unix" divergence.
        for (input, unix_kind, _is_prefix) in CORPUS {
            assert_eq!(
                classify_operand(OsStr::new(input)),
                *unix_kind,
                "classify_operand({input:?})"
            );
        }
    }

    #[test]
    #[cfg(windows)]
    fn classifier_treats_windows_prefixes_as_local() {
        // On Windows the drive-letter/UNC/device exemption is active, so every
        // Windows-shaped operand in the corpus is local while genuine remotes
        // stay remote. This runs in the Windows CI cell.
        for (input, _unix_kind, is_prefix) in CORPUS {
            let kind = classify_operand(OsStr::new(input));
            if *is_prefix {
                assert_eq!(kind, OperandKind::Local, "classify_operand({input:?})");
            }
        }
        assert_eq!(
            classify_operand(OsStr::new(r"C:\Windows\path")),
            OperandKind::Local
        );
        assert_eq!(
            classify_operand(OsStr::new("D:/Users/test")),
            OperandKind::Local
        );
    }

    #[test]
    fn daemon_vs_remote_distinction() {
        assert_eq!(
            classify_operand(OsStr::new("host::module")),
            OperandKind::Daemon
        );
        assert_eq!(
            classify_operand(OsStr::new("rsync://h/m")),
            OperandKind::Daemon
        );
        // An IPv6 literal's inner "::" must not be mistaken for a daemon module.
        assert_eq!(
            classify_operand(OsStr::new("[::1]:path")),
            OperandKind::Remote
        );
        assert_eq!(
            classify_operand(OsStr::new("ssh://host")),
            OperandKind::Remote
        );
    }

    #[test]
    fn is_remote_matches_both_non_local_kinds() {
        assert!(operand_is_remote(OsStr::new("host:path")));
        assert!(operand_is_remote(OsStr::new("host::module")));
        assert!(operand_is_remote(OsStr::new("rsync://h/m")));
        assert!(!operand_is_remote(OsStr::new("./local")));
        assert!(!operand_is_remote(OsStr::new("/foo:bar")));
    }
}
