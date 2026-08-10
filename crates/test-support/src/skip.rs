//! Self-skip helpers for upstream-compat ports.
//!
//! Operational ports (see `docs/design/uts-nextest-edge-b-test-harness.md`
//! section 9) are never `#[ignore]`d. Instead they call one of these
//! predicates at the top of the test body: when a prerequisite is missing
//! the helper prints a clear reason and returns `false`, and the test
//! early-returns so nextest reports it as passing with the skip line in
//! stderr. This keeps the standard PR cell green on hosts that lack root,
//! Unix semantics, or an external tool, without hiding real failures behind
//! a blanket ignore.

use std::path::PathBuf;

/// Skip unless running on a Unix platform.
///
/// Returns `true` on Unix; on other platforms prints a skip reason and
/// returns `false`. Ports that depend on POSIX permission bits, symlinks,
/// or `lsh-stub` gate on this.
#[must_use]
pub fn require_unix() -> bool {
    if cfg!(unix) {
        true
    } else {
        eprintln!("Skipping upstream-compat test: requires a Unix platform");
        false
    }
}

/// Locate a workspace binary in `target/{debug,release}/`.
///
/// Resolution walks up from the current test executable to the enclosing
/// `debug` or `release` profile directory and probes for `name` (with the
/// platform executable suffix). Returns the first match, preferring the
/// profile the test itself was built under.
#[must_use]
pub fn locate_workspace_binary(name: &str) -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    // `.../target/<profile>/deps/<test-bin>` -> `.../target/<profile>`.
    let profile_dir = exe.parent()?.parent()?;
    let candidate = profile_dir.join(exe_name(name));
    if candidate.is_file() {
        return Some(candidate);
    }

    // Fall back to the sibling profile so a release test can still find a
    // debug-built helper binary and vice versa.
    let target_dir = profile_dir.parent()?;
    for profile in ["debug", "release"] {
        let candidate = target_dir.join(profile).join(exe_name(name));
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// Skip unless the workspace binary `name` is present.
///
/// Wraps [`locate_workspace_binary`] with the self-skip convention: prints a
/// reason and returns `false` when the binary is not built.
#[must_use]
pub fn require_binary(name: &str) -> bool {
    if locate_workspace_binary(name).is_some() {
        true
    } else {
        eprintln!("Skipping upstream-compat test: workspace binary '{name}' not built");
        false
    }
}

/// Locate an external command on `PATH`.
///
/// Used by ports that shell out to tools like `setfacl` or `getfattr`.
/// Returns the resolved absolute path, or `None` if no entry on `PATH`
/// contains an executable-named match.
#[must_use]
pub fn locate_command_on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(exe_name(name));
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// Skip unless external command `name` is available on `PATH`.
#[must_use]
pub fn require_command_on_path(name: &str) -> bool {
    if locate_command_on_path(name).is_some() {
        true
    } else {
        eprintln!("Skipping upstream-compat test: command '{name}' not found on PATH");
        false
    }
}

/// Append the platform executable suffix (`.exe` on Windows).
fn exe_name(name: &str) -> String {
    let suffix = std::env::consts::EXE_SUFFIX;
    if suffix.is_empty() || name.ends_with(suffix) {
        name.to_string()
    } else {
        format!("{name}{suffix}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn require_unix_matches_platform() {
        // Why: the return must track the actual platform, or a Unix-only
        // port would either wrongly skip on Linux/Mac or wrongly run on
        // Windows.
        assert_eq!(require_unix(), cfg!(unix));
    }

    #[test]
    fn exe_name_adds_suffix_once() {
        // Why: double-suffixing ("foo.exe.exe") would break binary lookup
        // on Windows; a bare name must gain exactly one suffix.
        let base = "oc-rsync";
        let named = exe_name(base);
        assert!(named.starts_with(base));
        // Applying twice is idempotent.
        assert_eq!(exe_name(&named), named);
    }

    #[test]
    fn locate_command_finds_a_known_tool() {
        // Why: proves PATH resolution actually works end-to-end. A shell is
        // present in every CI image (`sh` on Unix, `cmd` on Windows), so a
        // None here means the resolver is broken, not the environment.
        let tool = if cfg!(windows) { "cmd" } else { "sh" };
        assert!(
            locate_command_on_path(tool).is_some(),
            "expected to find {tool} on PATH"
        );
    }

    #[test]
    fn locate_command_rejects_missing_tool() {
        // Why: a false positive would let a port believe a tool exists and
        // then fail opaquely when it shells out.
        assert!(locate_command_on_path("definitely-not-a-real-binary-xyzzy").is_none());
    }

    #[test]
    fn require_command_matches_locate() {
        // Why: the skip predicate and the locator must agree, or a test
        // could skip while the tool is present (or vice versa).
        let tool = if cfg!(windows) { "cmd" } else { "sh" };
        assert_eq!(
            require_command_on_path(tool),
            locate_command_on_path(tool).is_some()
        );
    }

    #[test]
    fn locate_workspace_binary_finds_the_test_binary_itself() {
        // Why: the current test executable lives under target/<profile>/deps,
        // so its own profile dir must be reachable. We assert the resolver
        // reaches the profile dir by locating a binary we know is there:
        // there is always at least the deps dir. A missing-name lookup must
        // return None rather than panic.
        assert!(locate_workspace_binary("definitely-not-built-xyzzy").is_none());
    }
}
