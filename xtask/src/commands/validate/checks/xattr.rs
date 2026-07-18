//! Deep user-xattr (`-X`) parity between oc-rsync and upstream.
//!
//! Where `acl_xattr` bundles ACLs with a token xattr, this check exercises
//! extended attributes alone and in depth: multiple keys on one file, an
//! empty-value key, a value with spaces and an `=` sign, a base64/binary value,
//! and a key on a directory. The fixture is pulled with each client over every
//! transport and oc's destination must be byte- and xattr-identical to
//! upstream's. Xattrs are compared per entry, path-independently (the `# file:`
//! header line is dropped), so readdir order never causes a false mismatch. The
//! whole check skips when the tools or work filesystem cannot carry user xattrs.

use std::path::{Path, PathBuf};

use crate::commands::validate::support;
use crate::commands::validate::transport::{Transport, pull_into};
use crate::commands::validate::{Check, CheckOutcome, ValidateCtx};

/// The deep user-xattr parity check.
pub struct Xattr;

/// Preserve standard metadata plus xattrs (no ACLs, so no `-A`).
const FLAGS: &[&str] = &["-rlptgoD", "-X", "--numeric-ids"];

/// External tools required to build the fixture and compare xattrs.
const TOOLS: &[&str] = &["setfattr", "getfattr"];

/// The three keys that must survive on `a.txt`, used for the non-vacuous guard.
const A_KEYS: &[&str] = &["user.one", "user.two", "user.three"];

impl Check for Xattr {
    fn name(&self) -> &'static str {
        "xattr"
    }

    fn run(&self, ctx: &ValidateCtx) -> Vec<CheckOutcome> {
        if !TOOLS.iter().all(|t| support::tool_available(t)) {
            return vec![CheckOutcome::skip(
                self.name(),
                "support",
                "setfattr/getfattr missing",
            )];
        }
        if !fs_supports_user_xattr(ctx.work) {
            return vec![CheckOutcome::skip(
                self.name(),
                "support",
                "filesystem does not support user xattrs",
            )];
        }

        let root = ctx.work.join("xattr");
        let src = root.join("src");
        if let Err(e) = build_fixture(&src) {
            return vec![CheckOutcome::skip(self.name(), "fixture", e)];
        }
        let expected = support::entry_count(&src);
        let flags: Vec<String> = FLAGS.iter().map(|s| s.to_string()).collect();

        ctx.transports
            .iter()
            .map(|&t| self.cell(ctx, t, &root, &flags, expected))
            .collect()
    }
}

impl Xattr {
    /// Run one transport cell: pull with both clients and compare xattrs.
    fn cell(
        &self,
        ctx: &ValidateCtx,
        transport: Transport,
        root: &Path,
        flags: &[String],
        expected: usize,
    ) -> CheckOutcome {
        let label = transport.label();
        if transport.needs_ssh() && !support::ssh_ready() {
            return CheckOutcome::skip(self.name(), label, "no sshd on localhost:22");
        }
        let src = root.join("src");
        let oc_dst = root.join(format!("oc-{label}"));
        let up_dst = root.join(format!("up-{label}"));

        let up = pull_into(
            transport.for_upstream(),
            ctx.upstream,
            ctx.upstream,
            &src,
            &up_dst,
            flags,
            ctx.work,
        );
        match up {
            Ok(out) if out.status.success() => {}
            other => return skip_or_fail(self.name(), label, "upstream", other),
        }
        let oc = pull_into(
            transport,
            ctx.oc,
            ctx.upstream,
            &src,
            &oc_dst,
            flags,
            ctx.work,
        );
        match oc {
            Ok(out) if out.status.success() => {}
            other => return skip_or_fail(self.name(), label, "oc", other),
        }

        // Genuine-result guard: both trees must be fully populated.
        if support::entry_count(&up_dst) != expected || support::entry_count(&oc_dst) != expected {
            return CheckOutcome::fail(self.name(), label, "destination entry count != source");
        }
        if let Some(diff) = support::content_diff(&oc_dst, &up_dst) {
            return CheckOutcome::fail(self.name(), label, diff);
        }
        // Non-vacuous guard: a bug that silently drops xattrs must be caught.
        let a_dump = xattr_of(&oc_dst.join("a.txt")).unwrap_or_default();
        if !A_KEYS.iter().all(|k| a_dump.contains(k)) {
            return CheckOutcome::fail(
                self.name(),
                label,
                "oc dest a.txt missing user.one/two/three",
            );
        }
        match xattr_diff(&oc_dst, &up_dst, ctx.verbose) {
            Some(rel) => CheckOutcome::fail(
                self.name(),
                label,
                format!("xattr differs at {}", rel.display()),
            ),
            None => CheckOutcome::pass(self.name(), label),
        }
    }
}

/// First per-entry xattr divergence between two trees, or `None` when identical.
/// Entries are walked in sorted order and compared path-independently. When
/// `verbose`, the two differing dumps are printed to stderr.
fn xattr_diff(oc: &Path, up: &Path, verbose: bool) -> Option<PathBuf> {
    for rel in support::rel_entries(oc) {
        let oc_dump = xattr_of(&oc.join(&rel));
        let up_dump = xattr_of(&up.join(&rel));
        if oc_dump != up_dump {
            if verbose {
                eprintln!("  xattr oc {}: {:?}", rel.display(), oc_dump);
                eprintln!("  xattr up {}: {:?}", rel.display(), up_dump);
            }
            return Some(rel);
        }
    }
    None
}

/// User xattrs of `path` with the path-bearing `# file:` line dropped.
fn xattr_of(path: &Path) -> Option<String> {
    let p = path.to_string_lossy();
    let raw = support::capture("getfattr", &["-d", "-m", "-", "--absolute-names", &p]).ok()?;
    Some(strip_file_line(&raw))
}

/// Drop the leading `# file:` header line from a `getfattr -d` dump, leaving a
/// path-independent list of `key="value"` lines.
fn strip_file_line(dump: &str) -> String {
    dump.lines()
        .filter(|line| !line.starts_with("# file:"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Probe whether `work`'s filesystem accepts a `user.*` xattr.
fn fs_supports_user_xattr(work: &Path) -> bool {
    let probe = work.join(".xattr_probe");
    if std::fs::write(&probe, b"x").is_err() {
        return false;
    }
    let ok = support::capture(
        "setfattr",
        &["-n", "user.probe", "-v", "1", &probe.to_string_lossy()],
    )
    .is_ok();
    let _ = std::fs::remove_file(&probe);
    ok
}

/// Distinguish a genuine divergence from an unrunnable cell (e.g. ssh refused).
fn skip_or_fail(
    check: &'static str,
    label: &str,
    who: &str,
    result: Result<std::process::Output, crate::error::TaskError>,
) -> CheckOutcome {
    match result {
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            let code = out.status.code().unwrap_or(-1);
            CheckOutcome::fail(
                check,
                label,
                format!("{who} exited {code}: {}", stderr.trim()),
            )
        }
        Err(e) => CheckOutcome::skip(check, label, format!("{who} could not run: {e}")),
    }
}

/// Build the xattr fixture. Idempotent: removes any prior tree first.
///
/// Layout: `a.txt` (three keys plus an empty-value key), `b.bin` (a spaced value
/// and a base64/binary value), `sub/c.txt`, a `sub` directory carrying a key,
/// and a `link` symlink to `a.txt`.
fn build_fixture(src: &Path) -> Result<(), String> {
    if src.exists() {
        std::fs::remove_dir_all(src).map_err(|e| e.to_string())?;
    }
    let sub = src.join("sub");
    std::fs::create_dir_all(&sub).map_err(|e| e.to_string())?;

    let a = src.join("a.txt");
    let b = src.join("b.bin");
    let c = sub.join("c.txt");
    std::fs::write(&a, b"alpha").map_err(|e| e.to_string())?;
    std::fs::write(&b, b"bravo").map_err(|e| e.to_string())?;
    std::fs::write(&c, b"charlie").map_err(|e| e.to_string())?;
    make_symlink("a.txt", &src.join("link")).map_err(|e| e.to_string())?;

    // Multiple keys on one file.
    setfattr(&["-n", "user.one", "-v", "alpha", &a.to_string_lossy()])?;
    setfattr(&["-n", "user.two", "-v", "beta", &a.to_string_lossy()])?;
    setfattr(&["-n", "user.three", "-v", "gamma", &a.to_string_lossy()])?;
    // Empty-value key (no `-v`).
    setfattr(&["-n", "user.empty", &a.to_string_lossy()])?;

    // A value with spaces and an `=` sign, and a base64/binary value.
    setfattr(&[
        "-n",
        "user.note",
        "-v",
        "hello world = test",
        &b.to_string_lossy(),
    ])?;
    setfattr(&[
        "-n",
        "user.bin",
        "-v",
        "0sAAECaGVsbG8=",
        &b.to_string_lossy(),
    ])?;

    // A key on the directory itself.
    setfattr(&[
        "-n",
        "user.dir",
        "-v",
        "on-a-directory",
        &sub.to_string_lossy(),
    ])?;

    // Backdate mtimes so the quick-check does not skip anything under test.
    for entry in support::rel_entries(src) {
        let path = src.join(&entry);
        support::capture(
            "touch",
            &["-h", "-d", "@1614830767", &path.to_string_lossy()],
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Set an xattr, surfacing failures as a fixture error.
fn setfattr(args: &[&str]) -> Result<(), String> {
    support::capture("setfattr", args)
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Create a symlink to `target` at `link` (no-op on non-unix platforms, where
/// this check never runs because `setfattr`/`getfattr` are absent).
#[cfg(unix)]
fn make_symlink(target: &str, link: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

/// Non-unix stub: the check skips before this matters.
#[cfg(not(unix))]
fn make_symlink(_target: &str, _link: &Path) -> std::io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::strip_file_line;

    #[test]
    fn strip_file_line_drops_only_the_path_header() {
        let dump = "# file: some/abs/path\nuser.one=\"alpha\"\nuser.two=\"beta\"";
        assert_eq!(
            strip_file_line(dump),
            "user.one=\"alpha\"\nuser.two=\"beta\""
        );
    }

    #[test]
    fn strip_file_line_is_path_independent() {
        let a = "# file: /work/oc-local/a.txt\nuser.one=\"alpha\"";
        let b = "# file: /work/up-local/a.txt\nuser.one=\"alpha\"";
        assert_eq!(strip_file_line(a), strip_file_line(b));
    }

    #[test]
    fn strip_file_line_keeps_value_lines_without_a_header() {
        let dump = "user.dir=\"on-a-directory\"";
        assert_eq!(strip_file_line(dump), dump);
    }
}
