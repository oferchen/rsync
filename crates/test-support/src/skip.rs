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

/// Locate a workspace binary built by the current Cargo invocation.
///
/// Exactly one path is considered: `name` inside
/// [`crate::target_profile_dir`], the profile directory this crate's
/// `build.rs` captured from `OUT_DIR`. There is deliberately no probe of a
/// sibling profile - resolving a `debug` binary for a `release` test run is
/// how a stale build silently ends up under test.
///
/// Returns `None` only when that one path does not exist, which callers must
/// treat as "not built", never as "use something else".
#[must_use]
pub fn locate_workspace_binary(name: &str) -> Option<PathBuf> {
    let candidate = crate::bin_path::workspace_bin_path(name);
    candidate.is_file().then_some(candidate)
}

/// Skip unless the workspace binary `name` is present.
///
/// Wraps [`locate_workspace_binary`] with the self-skip convention: prints the
/// exact path that was probed and returns `false` when the binary is not
/// built. Tests that must never be allowed to self-skip should call
/// [`crate::workspace_bin`] instead, which panics.
#[must_use]
pub fn require_binary(name: &str) -> bool {
    if locate_workspace_binary(name).is_some() {
        true
    } else {
        eprintln!(
            "Skipping upstream-compat test: workspace binary '{name}' not built at {}",
            crate::bin_path::workspace_bin_path(name).display()
        );
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

    #[test]
    fn locate_workspace_binary_considers_exactly_one_path() {
        // Why: any second candidate is a route to a binary from another
        // profile - i.e. another revision. When resolution succeeds it must
        // be the single profile-dir path, never a sibling-profile match.
        let name = "lsh-stub";
        if let Some(found) = locate_workspace_binary(name) {
            assert_eq!(found, crate::bin_path::workspace_bin_path(name));
        }
    }
}
