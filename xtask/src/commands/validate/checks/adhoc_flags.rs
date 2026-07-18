//! Ad-hoc arbitrary-flags parity.
//!
//! For each `--flags` scenario supplied on the command line, pull a standard
//! fixture with oc-rsync and with upstream using the exact same flag set over
//! every transport, and assert the two destinations are byte- and attribute-
//! identical. Because both sides run the identical flags, any divergence is
//! oc's. This lets any rsync option (or combination) be validated without
//! writing a new check.

use std::collections::BTreeMap;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use crate::commands::validate::support;
use crate::commands::validate::transport::{Transport, pull_into};
use crate::commands::validate::{Check, CheckOutcome, ValidateCtx};

/// The ad-hoc arbitrary-flags parity check.
pub struct AdHocFlags;

impl Check for AdHocFlags {
    fn name(&self) -> &'static str {
        "ad-hoc-flags"
    }

    fn run(&self, ctx: &ValidateCtx) -> Vec<CheckOutcome> {
        if ctx.flags.is_empty() {
            return Vec::new();
        }
        let root = ctx.work.join("ad-hoc-flags");
        let src = root.join("src");
        if let Err(e) = build_fixture(&src) {
            return vec![CheckOutcome::skip(self.name(), "fixture", e)];
        }
        let expected = support::entry_count(&src);

        let mut outcomes = Vec::new();
        for scenario in ctx.flags {
            let flags = parse_flags(scenario);
            for &transport in ctx.transports {
                outcomes.push(self.cell(ctx, transport, &root, &flags, scenario, expected));
            }
        }
        outcomes
    }
}

impl AdHocFlags {
    fn cell(
        &self,
        ctx: &ValidateCtx,
        transport: Transport,
        root: &Path,
        flags: &[String],
        scenario: &str,
        expected: usize,
    ) -> CheckOutcome {
        let cell = format!("[{scenario}] {}", transport.label());
        if transport.needs_ssh() && !support::ssh_ready() {
            return CheckOutcome::skip(self.name(), cell, "no sshd on localhost:22");
        }
        let src = root.join("src");
        let src = src.as_path();
        let slug = slug(scenario, transport.label());
        let oc_dst = root.join(format!("oc-{slug}"));
        let up_dst = root.join(format!("up-{slug}"));

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
        match attr_diff(&oc_dst, &up_dst) {
            Some(diff) => CheckOutcome::fail(self.name(), cell, diff),
            None => CheckOutcome::pass(self.name(), cell),
        }
    }
}

/// Split a whitespace-separated flag scenario into rsync arguments.
fn parse_flags(scenario: &str) -> Vec<String> {
    scenario.split_whitespace().map(str::to_string).collect()
}

/// A filesystem-safe slug for destination directory names.
fn slug(scenario: &str, label: &str) -> String {
    let cleaned: String = scenario
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    format!("{label}-{cleaned}")
}

/// Per-entry `(mode, mtime, uid, gid, nlink)` map for attribute comparison.
///
/// oc and upstream ran the identical flag set, so whatever those flags preserve
/// must match between the two destinations regardless of which flags they are.
fn attr_map(root: &Path) -> BTreeMap<std::path::PathBuf, (u32, i64, u32, u32, u64)> {
    support::rel_entries(root)
        .into_iter()
        .filter_map(|rel| {
            let meta = root.join(&rel).symlink_metadata().ok()?;
            Some((
                rel,
                (
                    meta.mode() & 0o7777,
                    meta.mtime(),
                    meta.uid(),
                    meta.gid(),
                    meta.nlink(),
                ),
            ))
        })
        .collect()
}

/// First per-attribute divergence between two trees, or `None` when identical.
fn attr_diff(oc: &Path, up: &Path) -> Option<String> {
    let (a, b) = (attr_map(oc), attr_map(up));
    let fields = ["perms", "mtime", "uid", "gid", "nlink"];
    for (rel, oc_attrs) in &a {
        let Some(up_attrs) = b.get(rel) else {
            return Some(format!("missing {} in oc tree", rel.display()));
        };
        let mismatches: Vec<&str> = fields
            .iter()
            .enumerate()
            .filter(|(i, _)| !attr_eq(*i, oc_attrs, up_attrs))
            .map(|(_, name)| *name)
            .collect();
        if !mismatches.is_empty() {
            return Some(format!(
                "{} differs at {}",
                mismatches.join("/"),
                rel.display()
            ));
        }
    }
    None
}

fn attr_eq(field: usize, a: &(u32, i64, u32, u32, u64), b: &(u32, i64, u32, u32, u64)) -> bool {
    match field {
        0 => a.0 == b.0,
        1 => a.1 == b.1,
        2 => a.2 == b.2,
        3 => a.3 == b.3,
        _ => a.4 == b.4,
    }
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

/// Build the standard fixture: varied perms, a subdir, a symlink, a hardlink
/// pair, and backdated mtimes so quick-check makes stable decisions.
fn build_fixture(src: &Path) -> Result<(), String> {
    if src.exists() {
        std::fs::remove_dir_all(src).map_err(|e| e.to_string())?;
    }
    let sub = src.join("sub");
    std::fs::create_dir_all(&sub).map_err(|e| e.to_string())?;

    write_mode(&src.join("f644"), b"alpha", 0o644)?;
    write_mode(&src.join("f600"), b"bravo", 0o600)?;
    write_mode(&sub.join("f640"), b"charlie", 0o640)?;
    std::fs::write(src.join("h1"), b"linked").map_err(|e| e.to_string())?;
    std::fs::hard_link(src.join("h1"), src.join("h2")).map_err(|e| e.to_string())?;
    std::os::unix::fs::symlink("../f644", sub.join("link")).map_err(|e| e.to_string())?;

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

fn write_mode(path: &Path, bytes: &[u8], mode: u32) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(path, bytes).map_err(|e| e.to_string())?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::{parse_flags, slug};

    #[test]
    fn parse_flags_splits_on_whitespace() {
        assert_eq!(
            parse_flags("-a  --sparse   --exclude=*.log"),
            vec!["-a", "--sparse", "--exclude=*.log"]
        );
        assert!(parse_flags("   ").is_empty());
    }

    #[test]
    fn slug_is_filesystem_safe() {
        assert_eq!(slug("-a --sparse", "local"), "local-_a___sparse");
        assert!(
            slug("--exclude=*.log", "russh")
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        );
    }
}
