//! Ownership (`--chown` / `--usermap` / `--groupmap`) parity between oc-rsync
//! and upstream.
//!
//! Builds a small fixture, then pulls it with each client over every transport
//! and asserts oc's destination owners (uid/gid) match upstream's byte-for-byte
//! after ownership remapping. Two scenarios run per transport:
//!
//! * `chown-self` - always runnable without privilege: `--chown=<uid>:<gid>`
//!   using the current effective id. Both clients target the same current id, so
//!   this validates that oc and upstream drive the `--chown` path identically.
//! * `usermap` - remaps ids to values only root may assign, so it is skipped
//!   unless the process runs as root. When runnable it proves oc and upstream
//!   apply `--usermap`/`--groupmap` the same way.
//!
//! Id remapping to arbitrary uids/gids requires root, so the check is
//! skip-aware and never assumes privilege.

use std::collections::BTreeMap;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use crate::commands::validate::support;
use crate::commands::validate::transport::{Transport, pull_into};
use crate::commands::validate::{Check, CheckOutcome, ValidateCtx};

/// The ownership parity check.
pub struct Chown;

/// Preserve owner and group so the `--chown`/`--*map` result is observable.
const BASE_FLAGS: &[&str] = &["-rlptgoD", "--numeric-ids"];

/// The current process identity, as reported by `id`.
struct Ids {
    /// Effective user id (`id -u`); the process is root iff this is `0`.
    euid: u32,
    /// Effective group id (`id -g`).
    gid: u32,
    /// Effective user name (`id -un`), retained for diagnostics.
    user: String,
    /// Effective group name (`id -gn`), retained for diagnostics.
    group: String,
}

/// One ownership scenario: the flags to run and the (uid, gid) they must yield.
struct Scenario<'a> {
    /// Cell label for the report.
    label: String,
    /// Full rsync flag set for the run.
    flags: &'a [String],
    /// Expected owning uid in both destinations.
    want_uid: u32,
    /// Expected owning gid in both destinations.
    want_gid: u32,
}

impl Check for Chown {
    fn name(&self) -> &'static str {
        "chown"
    }

    fn run(&self, ctx: &ValidateCtx) -> Vec<CheckOutcome> {
        let ids = match ids() {
            Ok(ids) => ids,
            Err(e) => return vec![CheckOutcome::skip(self.name(), "identity", e)],
        };
        if ctx.verbose {
            eprintln!(
                "[chown] running as {}:{} ({}:{})",
                ids.user, ids.group, ids.euid, ids.gid
            );
        }

        let root = ctx.work.join("chown");
        let src = root.join("src");
        if let Err(e) = build_fixture(&src) {
            return vec![CheckOutcome::skip(self.name(), "fixture", e)];
        }
        let expected = support::entry_count(&src);

        ctx.transports
            .iter()
            .flat_map(|&t| self.transport_cells(ctx, t, &root, &ids, expected))
            .collect()
    }
}

impl Chown {
    /// Both scenarios for one transport: the always-runnable `chown-self` and
    /// the root-only `usermap`.
    fn transport_cells(
        &self,
        ctx: &ValidateCtx,
        transport: Transport,
        root: &Path,
        ids: &Ids,
        expected: usize,
    ) -> Vec<CheckOutcome> {
        let label = transport.label();
        if transport.needs_ssh() && !support::ssh_ready() {
            return vec![
                CheckOutcome::skip(self.name(), format!("{label} chown-self"), "no sshd"),
                CheckOutcome::skip(self.name(), format!("{label} usermap"), "no sshd"),
            ];
        }

        // Scenario 1: chown to the current id. Non-root, always runnable.
        let chown_flags = flags(&[format!("--chown={}:{}", ids.euid, ids.gid)]);
        let self_cell = self.cell(
            ctx,
            transport,
            root,
            &Scenario {
                label: format!("{label} chown-self"),
                flags: &chown_flags,
                want_uid: ids.euid,
                want_gid: ids.gid,
            },
            expected,
        );

        // Scenario 2: remap ids to values only root may assign.
        let map_cell = if ids.euid != 0 {
            CheckOutcome::skip(
                self.name(),
                format!("{label} usermap"),
                "id remap needs root",
            )
        } else {
            let map_flags = flags(&["--usermap=0:1".to_string(), "--groupmap=0:1".to_string()]);
            self.cell(
                ctx,
                transport,
                root,
                &Scenario {
                    label: format!("{label} usermap"),
                    flags: &map_flags,
                    want_uid: 1,
                    want_gid: 1,
                },
                expected,
            )
        };

        vec![self_cell, map_cell]
    }

    /// Run one (transport, scenario) cell: pull with oc and upstream, then
    /// assert content parity, owner parity, and that the requested ids actually
    /// took effect.
    fn cell(
        &self,
        ctx: &ValidateCtx,
        transport: Transport,
        root: &Path,
        scenario: &Scenario,
        expected: usize,
    ) -> CheckOutcome {
        let cell = scenario.label.clone();
        let flags: &[String] = scenario.flags;
        let want_uid = scenario.want_uid;
        let want_gid = scenario.want_gid;
        let src = root.join("src");
        let src = src.as_path();
        let oc_dst = root.join(format!("oc-{}", sanitize(&cell)));
        let up_dst = root.join(format!("up-{}", sanitize(&cell)));

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
            other => return skip_or_fail(self.name(), &cell, "upstream", other),
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
            other => return skip_or_fail(self.name(), &cell, "oc", other),
        }

        // Genuine-result guard: both trees must be fully populated.
        if support::entry_count(&up_dst) != expected || support::entry_count(&oc_dst) != expected {
            return CheckOutcome::fail(self.name(), cell, "destination entry count != source");
        }
        if let Some(diff) = support::content_diff(&oc_dst, &up_dst) {
            return CheckOutcome::fail(self.name(), cell, diff);
        }

        // oc and upstream must apply the same ownership.
        if let Some(diff) = owner_diff(&oc_dst, &up_dst) {
            if ctx.verbose {
                dump_owners("oc", &oc_dst);
                dump_owners("upstream", &up_dst);
            }
            return CheckOutcome::fail(self.name(), cell, diff);
        }

        // Non-vacuous: the requested remap must actually be present in oc's tree.
        if let Some(diff) = owner_mismatch(&oc_dst, want_uid, want_gid) {
            if ctx.verbose {
                dump_owners("oc", &oc_dst);
            }
            return CheckOutcome::fail(
                self.name(),
                cell,
                format!("requested {want_uid}:{want_gid} not applied: {diff}"),
            );
        }

        CheckOutcome::pass(self.name(), cell)
    }
}

/// Combine [`BASE_FLAGS`] with scenario-specific flags into one owned vector.
fn flags(extra: &[String]) -> Vec<String> {
    BASE_FLAGS
        .iter()
        .map(|s| s.to_string())
        .chain(extra.iter().cloned())
        .collect()
}

/// Gather the effective identity via `id`.
fn ids() -> Result<Ids, String> {
    let euid = capture_u32("id", &["-u"])?;
    let gid = capture_u32("id", &["-g"])?;
    let user = support::capture("id", &["-un"]).map_err(|e| e.to_string())?;
    let group = support::capture("id", &["-gn"]).map_err(|e| e.to_string())?;
    Ok(Ids {
        euid,
        gid,
        user,
        group,
    })
}

/// Capture `program args...` and parse its trimmed stdout as a `u32`.
fn capture_u32(program: &str, args: &[&str]) -> Result<u32, String> {
    let out = support::capture(program, args).map_err(|e| e.to_string())?;
    out.trim()
        .parse()
        .map_err(|e| format!("parse `{program} {args:?}` output {out:?}: {e}"))
}

/// Map a tree to per-entry `(uid, gid)`, following no symlinks.
fn owner_map(root: &Path) -> BTreeMap<PathBuf, (u32, u32)> {
    support::rel_entries(root)
        .into_iter()
        .filter_map(|rel| {
            let meta = root.join(&rel).symlink_metadata().ok()?;
            Some((rel, (meta.uid(), meta.gid())))
        })
        .collect()
}

/// First per-entry ownership divergence between two trees, or `None` when every
/// entry's `(uid, gid)` matches.
fn owner_diff(oc: &Path, up: &Path) -> Option<String> {
    let (a, b) = (owner_map(oc), owner_map(up));
    for (rel, oc_owner) in &a {
        let Some(up_owner) = b.get(rel) else {
            return Some(format!("missing {} in upstream tree", rel.display()));
        };
        if oc_owner != up_owner {
            return Some(format!(
                "owner differs at {}: oc {}:{} vs upstream {}:{}",
                rel.display(),
                oc_owner.0,
                oc_owner.1,
                up_owner.0,
                up_owner.1
            ));
        }
    }
    for rel in b.keys() {
        if !a.contains_key(rel) {
            return Some(format!("missing {} in oc tree", rel.display()));
        }
    }
    None
}

/// First entry whose owner is not `(want_uid, want_gid)`, or `None` when the
/// whole tree carries the expected ownership.
fn owner_mismatch(root: &Path, want_uid: u32, want_gid: u32) -> Option<String> {
    for (rel, (uid, gid)) in owner_map(root) {
        if uid != want_uid || gid != want_gid {
            return Some(format!("{} is {uid}:{gid}", rel.display()));
        }
    }
    None
}

/// Print a tree's per-entry `(uid, gid)` for verbose failure diagnosis.
fn dump_owners(who: &str, root: &Path) {
    for (rel, (uid, gid)) in owner_map(root) {
        eprintln!("[chown] {who} {} -> {uid}:{gid}", rel.display());
    }
}

/// Turn a cell label into a filesystem-safe destination suffix.
fn sanitize(cell: &str) -> String {
    cell.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// Distinguish a genuine divergence from an unrunnable cell (e.g. ssh refused).
fn skip_or_fail(
    check: &'static str,
    cell: &str,
    who: &str,
    result: Result<std::process::Output, crate::error::TaskError>,
) -> CheckOutcome {
    match result {
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            let code = out.status.code().unwrap_or(-1);
            CheckOutcome::fail(
                check,
                cell.to_string(),
                format!("{who} exited {code}: {}", stderr.trim()),
            )
        }
        Err(e) => CheckOutcome::skip(check, cell.to_string(), format!("{who} could not run: {e}")),
    }
}

/// Build the ownership fixture: two top-level files plus a subdirectory holding
/// one file. Idempotent - removes any prior tree first. Mtimes are backdated so
/// the quick-check does not skip the transfer under test.
fn build_fixture(src: &Path) -> Result<(), String> {
    if src.exists() {
        std::fs::remove_dir_all(src).map_err(|e| e.to_string())?;
    }
    let sub = src.join("sub");
    std::fs::create_dir_all(&sub).map_err(|e| e.to_string())?;

    std::fs::write(src.join("alpha"), b"alpha").map_err(|e| e.to_string())?;
    std::fs::write(src.join("bravo"), b"bravo").map_err(|e| e.to_string())?;
    std::fs::write(sub.join("charlie"), b"charlie").map_err(|e| e.to_string())?;

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

#[cfg(test)]
mod tests {
    use super::{owner_diff, owner_mismatch, sanitize};
    use std::fs;
    use std::os::unix::fs::MetadataExt;

    #[test]
    fn owner_diff_none_for_hard_linked_identical_ownership() {
        // A hard link shares the inode, so uid/gid are guaranteed identical
        // without touching ownership (which would need root).
        let (a, b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let source = a.path().join("f");
        fs::write(&source, b"x").unwrap();
        fs::hard_link(&source, b.path().join("f")).unwrap();
        assert!(owner_diff(a.path(), b.path()).is_none());
    }

    #[test]
    fn owner_diff_reports_missing_entry() {
        // A structural divergence exercises the Some path without needing root
        // to synthesize a uid/gid mismatch.
        let (a, b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        fs::write(a.path().join("only"), b"x").unwrap();
        assert!(owner_diff(a.path(), b.path()).unwrap().contains("missing"));
    }

    #[test]
    fn owner_mismatch_flags_unexpected_owner() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f");
        fs::write(&path, b"x").unwrap();
        let meta = path.symlink_metadata().unwrap();
        // The file is owned by us; requesting our own id must pass, and an
        // impossible id must be flagged.
        assert!(owner_mismatch(dir.path(), meta.uid(), meta.gid()).is_none());
        let bogus = meta.uid().wrapping_add(1);
        assert!(owner_mismatch(dir.path(), bogus, meta.gid()).is_some());
    }

    #[test]
    fn sanitize_keeps_alphanumerics_only() {
        assert_eq!(sanitize("local chown-self"), "local-chown-self");
    }
}
