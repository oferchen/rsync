//! Sparse-file parity between oc-rsync and upstream under `-S`/`--sparse`.
//!
//! Builds a fixture holding a mostly-hole file (`holey.dat`) plus a small dense
//! control (`dense.txt`), then pulls it with each client over every transport
//! and asserts oc's destination is byte-identical to upstream's *and* that oc
//! preserved the hole: its allocated block count stays close to upstream's and
//! far below the file's logical size, proving it did not write the hole as
//! zeros. When the work filesystem refuses to store the source sparsely the
//! whole check degrades to a single skip.

use std::os::unix::fs::MetadataExt;
use std::path::Path;

use crate::commands::validate::support;
use crate::commands::validate::transport::{Transport, pull_into};
use crate::commands::validate::{Check, CheckOutcome, ValidateCtx};

/// The sparse-file parity check.
pub struct Sparse;

/// Preserve metadata a non-root user can and request sparse writes with `-S`.
const FLAGS: &[&str] = &["-rlptgoD", "-S", "--numeric-ids"];

/// Logical hole size, in bytes, punched into `holey.dat` (1 MiB). Large enough
/// that a client writing the hole as zeros allocates dramatically more blocks
/// than one that leaves it unwritten.
const HOLE: u64 = 1 << 20;

/// Slack, in 512-byte `blocks()` units, allowed between two equivalently sparse
/// copies. One 4 KiB filesystem block is eight units; this permits a couple of
/// blocks of allocator/rounding difference between the two clients while still
/// rejecting a copy that materialised the whole hole.
const BLOCK_TOLERANCE: u64 = 16;

impl Check for Sparse {
    fn name(&self) -> &'static str {
        "sparse"
    }

    fn run(&self, ctx: &ValidateCtx) -> Vec<CheckOutcome> {
        let root = ctx.work.join("sparse");
        let src = root.join("src");
        if let Err(e) = build_fixture(&src) {
            return vec![CheckOutcome::skip(self.name(), "fixture", e)];
        }
        match sparse_support(&src.join("holey.dat")) {
            Ok(true) => {}
            Ok(false) => {
                return vec![CheckOutcome::skip(
                    self.name(),
                    "support",
                    "work filesystem does not store sparse files",
                )];
            }
            Err(e) => return vec![CheckOutcome::skip(self.name(), "support", e)],
        }
        let expected = support::entry_count(&src);
        let flags: Vec<String> = FLAGS.iter().map(|s| s.to_string()).collect();

        ctx.transports
            .iter()
            .map(|&t| self.cell(ctx, t, &root, &flags, expected))
            .collect()
    }
}

impl Sparse {
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
        // Logical bytes (holes included) must be identical to upstream.
        if let Some(diff) = support::content_diff(&oc_dst, &up_dst) {
            return CheckOutcome::fail(self.name(), label, diff);
        }

        let (o_blocks, sz) = match blocks_and_len(&oc_dst.join("holey.dat")) {
            Ok(v) => v,
            Err(e) => return CheckOutcome::fail(self.name(), label, e),
        };
        let (u_blocks, _) = match blocks_and_len(&up_dst.join("holey.dat")) {
            Ok(v) => v,
            Err(e) => return CheckOutcome::fail(self.name(), label, e),
        };
        if ctx.verbose {
            eprintln!(
                "[sparse/{label}] oc blocks={o_blocks} size={sz}; upstream blocks={u_blocks}"
            );
        }
        // Sparseness proof: oc allocated far below the logical size, and stayed
        // within filesystem-rounding tolerance of upstream's block count.
        if is_stored_sparse(o_blocks, sz) && blocks_within_tolerance(o_blocks, u_blocks) {
            CheckOutcome::pass(self.name(), label)
        } else {
            CheckOutcome::fail(
                self.name(),
                label,
                format!("blocks oc={o_blocks} upstream={u_blocks} size={sz}"),
            )
        }
    }
}

/// True when a `len`-byte file occupying `blocks` 512-byte units is physically
/// sparse: its allocated bytes are strictly fewer than its logical size, so at
/// least one hole was left unwritten.
fn is_stored_sparse(blocks: u64, len: u64) -> bool {
    blocks.saturating_mul(512) < len
}

/// True when two allocated-block counts differ by at most [`BLOCK_TOLERANCE`]
/// 512-byte units - i.e. the two copies are equivalently sparse aside from
/// filesystem rounding.
fn blocks_within_tolerance(oc_blocks: u64, up_blocks: u64) -> bool {
    oc_blocks.abs_diff(up_blocks) <= BLOCK_TOLERANCE
}

/// Probe whether the work filesystem stored the source `holey.dat` sparsely.
fn sparse_support(holey: &Path) -> Result<bool, String> {
    let meta = holey.symlink_metadata().map_err(|e| e.to_string())?;
    Ok(is_stored_sparse(meta.blocks(), meta.len()))
}

/// Allocated 512-byte block count and logical length of `path`.
fn blocks_and_len(path: &Path) -> Result<(u64, u64), String> {
    let meta = path.symlink_metadata().map_err(|e| e.to_string())?;
    Ok((meta.blocks(), meta.len()))
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

/// Build the sparse fixture. Idempotent: removes any prior tree first. Writes a
/// mostly-hole file whose logical size is ~1 MiB but whose written data is a
/// few bytes at each end, plus a small dense control file.
fn build_fixture(src: &Path) -> Result<(), String> {
    if src.exists() {
        std::fs::remove_dir_all(src).map_err(|e| e.to_string())?;
    }
    std::fs::create_dir_all(src).map_err(|e| e.to_string())?;

    write_holey(&src.join("holey.dat"))?;
    std::fs::write(src.join("dense.txt"), b"dense control payload\n").map_err(|e| e.to_string())?;

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

/// Create a genuinely sparse file: a few header bytes, a [`HOLE`]-byte gap left
/// unwritten via `seek`, then a few trailer bytes. Logical size is `HOLE + 8`
/// but only the two ends occupy blocks.
fn write_holey(path: &Path) -> Result<(), String> {
    use std::io::{Seek, SeekFrom, Write};
    let mut f = std::fs::File::create(path).map_err(|e| e.to_string())?;
    f.write_all(b"HEAD\0\0\0\0").map_err(|e| e.to_string())?;
    f.seek(SeekFrom::Start(HOLE)).map_err(|e| e.to_string())?;
    f.write_all(b"TAILTAIL").map_err(|e| e.to_string())?;
    f.sync_all().map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{BLOCK_TOLERANCE, blocks_within_tolerance, is_stored_sparse};

    #[test]
    fn sparse_only_when_allocated_bytes_below_logical_size() {
        // 8 units * 512 = 4 KiB allocated for a 1 MiB logical file -> sparse.
        assert!(is_stored_sparse(8, 1 << 20));
        // Allocating the full length (2048 * 512 == 1 MiB) is not sparse.
        assert!(!is_stored_sparse(2048, 1 << 20));
        // Boundary: exactly len allocated is dense, not sparse.
        assert!(!is_stored_sparse(2, 1024));
    }

    #[test]
    fn tolerance_absorbs_rounding_but_rejects_a_filled_hole() {
        assert!(blocks_within_tolerance(8, 8));
        assert!(blocks_within_tolerance(8, 8 + BLOCK_TOLERANCE));
        assert!(blocks_within_tolerance(8 + BLOCK_TOLERANCE, 8));
        assert!(!blocks_within_tolerance(8, 8 + BLOCK_TOLERANCE + 1));
        // A copy that wrote the 1 MiB hole as zeros allocates far more blocks.
        assert!(!blocks_within_tolerance(2048, 8));
    }
}
