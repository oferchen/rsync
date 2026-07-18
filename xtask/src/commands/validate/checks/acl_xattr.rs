//! ACL (`-A`) and xattr (`-X`) parity between oc-rsync and upstream.
//!
//! Builds a fixture carrying a named-user ACL, a default ACL on a subdir, and
//! user xattrs, then pulls it with each client over every transport and asserts
//! oc's destination is byte- and attribute-identical to upstream's. ACLs and
//! xattrs are compared per entry, path-independently (the ACL/xattr dumps drop
//! their path-bearing header lines), so readdir-order never causes a false
//! mismatch. The whole check skips when the tools or work filesystem cannot
//! carry ACLs and xattrs.

use std::path::Path;

use crate::commands::validate::support;
use crate::commands::validate::transport::{Transport, pull_into};
use crate::commands::validate::{Check, CheckOutcome, ValidateCtx};

/// The ACL + xattr parity check.
pub struct AclXattr;

/// Preserve standard metadata plus ACLs and xattrs.
const FLAGS: &[&str] = &["-rlptgoD", "-A", "-X", "-H", "--numeric-ids"];

/// External tools required to build the fixture and compare attributes.
const TOOLS: &[&str] = &["setfacl", "getfacl", "setfattr", "getfattr"];

impl Check for AclXattr {
    fn name(&self) -> &'static str {
        "acl-xattr"
    }

    fn run(&self, ctx: &ValidateCtx) -> Vec<CheckOutcome> {
        if !TOOLS.iter().all(|t| support::tool_available(t)) {
            return vec![CheckOutcome::skip(
                self.name(),
                "support",
                "setfacl/getfacl/setfattr/getfattr missing",
            )];
        }
        let uid = match current_uid() {
            Some(uid) => uid,
            None => {
                return vec![CheckOutcome::skip(
                    self.name(),
                    "support",
                    "cannot determine uid",
                )];
            }
        };
        if !fs_supports_acl_xattr(ctx.work, uid) {
            return vec![CheckOutcome::skip(
                self.name(),
                "support",
                "ACL/xattr unsupported on work fs",
            )];
        }

        let root = ctx.work.join("acl-xattr");
        let src = root.join("src");
        if let Err(e) = build_fixture(&src, uid) {
            return vec![CheckOutcome::skip(self.name(), "fixture", e)];
        }
        let expected = support::entry_count(&src);
        let flags: Vec<String> = FLAGS.iter().map(|s| s.to_string()).collect();

        ctx.transports
            .iter()
            .map(|&t| self.cell(ctx, t, &root, &src, &flags, expected))
            .collect()
    }
}

impl AclXattr {
    /// Run one transport cell: pull with both clients and compare destinations.
    fn cell(
        &self,
        ctx: &ValidateCtx,
        transport: Transport,
        root: &Path,
        src: &Path,
        flags: &[String],
        expected: usize,
    ) -> CheckOutcome {
        let label = transport.label();
        if transport.needs_ssh() && !support::ssh_ready() {
            return CheckOutcome::skip(self.name(), label, "no sshd on localhost:22");
        }
        let oc_dst = root.join(format!("oc-{label}"));
        let up_dst = root.join(format!("up-{label}"));

        let up = pull_into(
            transport.for_upstream(),
            ctx.upstream,
            ctx.upstream,
            src,
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
            src,
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
        match attr_diff(&oc_dst, &up_dst) {
            Some(diff) => CheckOutcome::fail(self.name(), label, diff),
            None => CheckOutcome::pass(self.name(), label),
        }
    }
}

/// First per-entry ACL or xattr divergence between two trees, or `None` when
/// identical. Entries are walked in sorted order and compared path-independently.
fn attr_diff(oc: &Path, up: &Path) -> Option<String> {
    for rel in support::rel_entries(oc) {
        let (oc_path, up_path) = (oc.join(&rel), up.join(&rel));
        if acl_of(&oc_path) != acl_of(&up_path) {
            return Some(format!("ACL differs at {}", rel.display()));
        }
        if xattr_of(&oc_path) != xattr_of(&up_path) {
            return Some(format!("xattr differs at {}", rel.display()));
        }
    }
    None
}

/// Numeric ACL of `path` without the path-bearing header comment lines.
fn acl_of(path: &Path) -> Option<String> {
    // `-c` omits the `# file:`/`# owner:` header, `-n` keeps ids numeric.
    support::capture("getfacl", &["-c", "-n", &path.to_string_lossy()]).ok()
}

/// User xattrs of `path` with the path-bearing `# file:` line dropped.
fn xattr_of(path: &Path) -> Option<String> {
    let raw = support::capture("getfattr", &["-d", "-m", "-", &path.to_string_lossy()]).ok()?;
    Some(
        raw.lines()
            .filter(|line| !line.starts_with("# file:"))
            .collect::<Vec<_>>()
            .join("\n"),
    )
}

/// The current user's numeric uid via `id -u`.
fn current_uid() -> Option<u32> {
    support::capture("id", &["-u"]).ok()?.trim().parse().ok()
}

/// Probe whether `work`'s filesystem accepts an ACL and a user xattr.
fn fs_supports_acl_xattr(work: &Path, uid: u32) -> bool {
    let probe = work.join(".acl_xattr_probe");
    if std::fs::write(&probe, b"x").is_err() {
        return false;
    }
    let acl = support::capture(
        "setfacl",
        &["-m", &format!("u:{uid}:rwx"), &probe.to_string_lossy()],
    )
    .is_ok();
    let xattr = support::capture(
        "setfattr",
        &["-n", "user.oc_probe", "-v", "1", &probe.to_string_lossy()],
    )
    .is_ok();
    let _ = std::fs::remove_file(&probe);
    acl && xattr
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

/// Build the ACL/xattr fixture. Idempotent: removes any prior tree first.
fn build_fixture(src: &Path, uid: u32) -> Result<(), String> {
    if src.exists() {
        std::fs::remove_dir_all(src).map_err(|e| e.to_string())?;
    }
    let sub = src.join("sub");
    std::fs::create_dir_all(&sub).map_err(|e| e.to_string())?;

    let a = src.join("a.txt");
    let b = sub.join("b.txt");
    std::fs::write(&a, b"alpha").map_err(|e| e.to_string())?;
    std::fs::write(&b, b"bravo").map_err(|e| e.to_string())?;

    // Named-user ACL on a file and a default ACL on the subdir.
    setfacl(&["-m", &format!("u:{uid}:rwx"), &a.to_string_lossy()])?;
    setfacl(&["-d", "-m", &format!("u:{uid}:rx"), &sub.to_string_lossy()])?;

    // User xattrs on files.
    setfattr(&["-n", "user.foo", "-v", "bar", &a.to_string_lossy()])?;
    setfattr(&["-n", "user.baz", "-v", "qux", &b.to_string_lossy()])?;

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

/// Apply an ACL edit, surfacing failures as a fixture error.
fn setfacl(args: &[&str]) -> Result<(), String> {
    support::capture("setfacl", args)
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Set an xattr, surfacing failures as a fixture error.
fn setfattr(args: &[&str]) -> Result<(), String> {
    support::capture("setfattr", args)
        .map(|_| ())
        .map_err(|e| e.to_string())
}
