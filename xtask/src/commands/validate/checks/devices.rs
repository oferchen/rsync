//! Special-file and device-node parity between oc-rsync and upstream.
//!
//! Builds a fixture holding a regular control file, a FIFO, and a unix socket -
//! plus a character and a block device node when running as root - then pulls it
//! with each client over every transport using `-rlptgoD` (the `-D` requests
//! `--devices --specials`). It asserts oc's destination reproduces every entry's
//! file *type* exactly as upstream does, and that device nodes carry the same
//! `rdev` (major/minor).
//!
//! FIFO and socket specials are validated for any user. Character/block device
//! validation requires `--root` (and an actual euid of 0): unprivileged users
//! cannot create device nodes via `mknod`, so that portion is omitted and the
//! check still passes on the FIFO/socket specials it can exercise.

use std::collections::BTreeMap;
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::{Path, PathBuf};

use crate::commands::validate::support;
use crate::commands::validate::transport::{Transport, pull_into};
use crate::commands::validate::{Check, CheckOutcome, ValidateCtx};

/// The special-file / device-node parity check.
///
/// Character and block device validation is only performed with `--root`; see
/// the module documentation for the privilege gate.
pub struct Devices;

/// `-D` (inside `-rlptgoD`) is `--devices --specials`; `--numeric-ids` keeps
/// ownership comparisons independent of the local passwd/group databases.
const FLAGS: &[&str] = &["-rlptgoD", "--numeric-ids"];

/// A file entry's transferable kind, as observed by `lstat`.
///
/// Device variants carry the raw `rdev` so equality captures both the type and
/// the major/minor pair without depending on a platform-specific decode.
#[derive(Debug, PartialEq, Eq)]
enum Special {
    Regular,
    Dir,
    Symlink,
    Fifo,
    Socket,
    CharDev(u64),
    BlockDev(u64),
    Other(u32),
}

impl Check for Devices {
    fn name(&self) -> &'static str {
        "devices"
    }

    fn run(&self, ctx: &ValidateCtx) -> Vec<CheckOutcome> {
        let root = ctx.work.join("devices");
        let src = root.join("src");
        // Device nodes need real root: honor the caller's --root request only
        // when the process is actually euid 0.
        let make_devices = ctx.root && current_euid() == Some(0);
        if ctx.verbose && !make_devices {
            eprintln!("    devices: omitting char/block nodes (requires --root and euid 0)");
        }
        if let Err(e) = build_fixture(&src, make_devices) {
            return vec![CheckOutcome::skip(self.name(), "fixture", e)];
        }
        let flags: Vec<String> = FLAGS.iter().map(|s| s.to_string()).collect();

        ctx.transports
            .iter()
            .map(|&t| self.cell(ctx, t, &root, &flags, make_devices))
            .collect()
    }
}

impl Devices {
    fn cell(
        &self,
        ctx: &ValidateCtx,
        transport: Transport,
        root: &Path,
        flags: &[String],
        has_devices: bool,
    ) -> CheckOutcome {
        let src = root.join("src");
        let expected = support::entry_count(&src);
        let label = transport.label();
        if transport.needs_ssh() && !support::ssh_ready() {
            return CheckOutcome::skip(self.name(), label, "no sshd on localhost:22");
        }
        let oc_dst = root.join(format!("oc-{label}"));
        let up_dst = root.join(format!("up-{label}"));

        let up = match pull_into(
            transport.for_upstream(),
            ctx.upstream,
            ctx.upstream,
            &src,
            &up_dst,
            flags,
            ctx.work,
        ) {
            Ok(out) if out.status.success() => out,
            other => return skip_or_fail(self.name(), label, "upstream", other),
        };
        let _ = up;
        let oc = match pull_into(
            transport,
            ctx.oc,
            ctx.upstream,
            &src,
            &oc_dst,
            flags,
            ctx.work,
        ) {
            Ok(out) if out.status.success() => out,
            other => return skip_or_fail(self.name(), label, "oc", other),
        };
        let _ = oc;

        // Genuine-result guard: both trees must be fully populated.
        if support::entry_count(&up_dst) != expected || support::entry_count(&oc_dst) != expected {
            return CheckOutcome::fail(self.name(), label, "destination entry count != source");
        }
        // Non-vacuous guard: oc's destination must actually carry the specials,
        // so a bug that silently drops FIFOs or device nodes still fails here.
        if let Some(missing) = oc_special_missing(&oc_dst, &src, has_devices) {
            return CheckOutcome::fail(self.name(), label, missing);
        }
        match special_diff(&oc_dst, &up_dst) {
            Some(diff) => CheckOutcome::fail(self.name(), label, diff),
            None => CheckOutcome::pass(self.name(), label),
        }
    }
}

/// Classify `path` by its own type (never following a symlink).
fn classify(path: &Path) -> Option<Special> {
    let meta = path.symlink_metadata().ok()?;
    let ft = meta.file_type();
    Some(if ft.is_fifo() {
        Special::Fifo
    } else if ft.is_socket() {
        Special::Socket
    } else if ft.is_char_device() {
        Special::CharDev(meta.rdev())
    } else if ft.is_block_device() {
        Special::BlockDev(meta.rdev())
    } else if ft.is_symlink() {
        Special::Symlink
    } else if ft.is_dir() {
        Special::Dir
    } else if ft.is_file() {
        Special::Regular
    } else {
        Special::Other(meta.mode() & 0o170000)
    })
}

/// Map a tree to per-entry [`Special`] for comparison.
fn special_map(root: &Path) -> BTreeMap<PathBuf, Special> {
    support::rel_entries(root)
        .into_iter()
        .filter_map(|rel| classify(&root.join(&rel)).map(|kind| (rel, kind)))
        .collect()
}

/// First special-type (or device major/minor) divergence between two trees, or
/// `None` when every entry matches.
fn special_diff(oc: &Path, up: &Path) -> Option<String> {
    let (a, b) = (special_map(oc), special_map(up));
    for (rel, oc_kind) in &a {
        match b.get(rel) {
            None => return Some(format!("missing {} in upstream tree", rel.display())),
            Some(up_kind) if up_kind != oc_kind => {
                return Some(format!(
                    "special type differs at {}: oc={} upstream={}",
                    rel.display(),
                    describe(oc_kind),
                    describe(up_kind)
                ));
            }
            Some(_) => {}
        }
    }
    None
}

/// Report the first special that failed to arrive in `oc_dst`, or `None` when
/// the FIFO (and, with devices, the char device) are present and correct.
fn oc_special_missing(oc_dst: &Path, src: &Path, has_devices: bool) -> Option<String> {
    match classify(&oc_dst.join("fifo")) {
        Some(Special::Fifo) => {}
        other => {
            return Some(format!(
                "fifo not preserved in oc destination (got {})",
                describe_opt(other)
            ));
        }
    }
    if has_devices {
        let want = classify(&src.join("cdev"));
        match (classify(&oc_dst.join("cdev")), want) {
            (Some(Special::CharDev(got)), Some(Special::CharDev(exp))) if got == exp => {}
            (got, _) => {
                return Some(format!(
                    "cdev not preserved as char device in oc destination (got {})",
                    describe_opt(got)
                ));
            }
        }
    }
    None
}

/// Human-readable label for a [`Special`], decoding device major/minor.
fn describe(kind: &Special) -> String {
    match kind {
        Special::Regular => "regular".to_string(),
        Special::Dir => "directory".to_string(),
        Special::Symlink => "symlink".to_string(),
        Special::Fifo => "fifo".to_string(),
        Special::Socket => "socket".to_string(),
        Special::CharDev(rdev) => {
            let (major, minor) = rdev_parts(*rdev);
            format!("char-device {major},{minor}")
        }
        Special::BlockDev(rdev) => {
            let (major, minor) = rdev_parts(*rdev);
            format!("block-device {major},{minor}")
        }
        Special::Other(bits) => format!("other ({bits:#o})"),
    }
}

/// Describe an optional kind, rendering `None` as `absent`.
fn describe_opt(kind: Option<Special>) -> String {
    kind.map(|k| describe(&k))
        .unwrap_or_else(|| "absent".to_string())
}

/// Split a device `rdev` into `(major, minor)` for display.
///
/// Uses the classic 8-bit-minor encoding, which is exact for the small
/// major/minor pairs this check creates (`1,3` and `7,0`). Pass/fail comparisons
/// use the raw `rdev` value, so this decode only affects diagnostics.
fn rdev_parts(rdev: u64) -> (u64, u64) {
    ((rdev >> 8) & 0xff, rdev & 0xff)
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

/// Query the effective uid via `id -u`; `None` when it cannot be parsed.
fn current_euid() -> Option<u32> {
    support::capture("id", &["-u"])
        .ok()
        .and_then(|s| s.trim().parse().ok())
}

/// Build the devices fixture. Idempotent: removes any prior tree first. Creates
/// character (`1,3`) and block (`7,0`) device nodes only when `make_devices`.
fn build_fixture(src: &Path, make_devices: bool) -> Result<(), String> {
    if src.exists() {
        std::fs::remove_dir_all(src).map_err(|e| e.to_string())?;
    }
    std::fs::create_dir_all(src).map_err(|e| e.to_string())?;

    std::fs::write(src.join("plain.txt"), b"control").map_err(|e| e.to_string())?;

    // FIFO: mkfifo works as an ordinary user.
    let fifo = src.join("fifo");
    let fifo_arg = fifo.to_string_lossy();
    support::capture("mkfifo", &[fifo_arg.as_ref()]).map_err(|e| e.to_string())?;

    // Socket: binding then dropping leaves the socket inode on disk (Rust's
    // UnixListener does not unlink on drop). upstream rsync.h treats S_ISSOCK as
    // a special, so --specials transfers it like a FIFO.
    let sock = src.join("sock");
    {
        let _listener = std::os::unix::net::UnixListener::bind(&sock).map_err(|e| e.to_string())?;
    }

    if make_devices {
        let cdev = src.join("cdev");
        let cdev_arg = cdev.to_string_lossy();
        support::capture("mknod", &[cdev_arg.as_ref(), "c", "1", "3"])
            .map_err(|e| e.to_string())?;
        let bdev = src.join("bdev");
        let bdev_arg = bdev.to_string_lossy();
        support::capture("mknod", &[bdev_arg.as_ref(), "b", "7", "0"])
            .map_err(|e| e.to_string())?;
    }

    // Backdate mtimes so the quick-check does not skip anything under test. A
    // socket may reject the timestamp update; skip it rather than fail.
    for rel in support::rel_entries(src) {
        let path = src.join(&rel);
        let is_socket = path
            .symlink_metadata()
            .map(|m| m.file_type().is_socket())
            .unwrap_or(false);
        match support::capture(
            "touch",
            &["-h", "-d", "@1614830767", &path.to_string_lossy()],
        ) {
            Ok(_) => {}
            Err(e) => {
                if is_socket {
                    continue;
                }
                return Err(e.to_string());
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{Special, describe, rdev_parts};

    #[test]
    fn rdev_parts_splits_classic_major_minor() {
        // 0x0103 == mknod c 1 3, 0x0700 == mknod b 7 0.
        assert_eq!(rdev_parts(0x0103), (1, 3));
        assert_eq!(rdev_parts(0x0700), (7, 0));
    }

    #[test]
    fn special_equality_distinguishes_kind_and_rdev() {
        assert_eq!(Special::Fifo, Special::Fifo);
        assert_ne!(Special::Fifo, Special::Socket);
        // Same kind but a different device number is a genuine divergence.
        assert_ne!(Special::CharDev(0x0103), Special::CharDev(0x0104));
        assert_eq!(Special::CharDev(0x0103), Special::CharDev(0x0103));
    }

    #[test]
    fn describe_names_device_major_minor() {
        assert_eq!(describe(&Special::CharDev(0x0103)), "char-device 1,3");
        assert_eq!(describe(&Special::BlockDev(0x0700)), "block-device 7,0");
        assert_eq!(describe(&Special::Fifo), "fifo");
    }
}
