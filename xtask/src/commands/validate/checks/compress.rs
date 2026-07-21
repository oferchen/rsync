//! Codec transparency between oc-rsync and upstream.
//!
//! Compression only ever touches the wire - the delivered bytes must equal the
//! source. This builds a fixture that stresses the compressor (a highly
//! compressible file, plain text, an incompressible blob, and a nested file),
//! then pulls it with `-z` and with `-z --compress-level=9` over every transport
//! and asserts oc's destination is byte-identical to both upstream's and the
//! source. That proves oc's codec round-trips exactly like upstream's.

use std::path::Path;

use crate::commands::validate::support;
use crate::commands::validate::transport::{Transport, pull_into};
use crate::commands::validate::{Check, CheckOutcome, ValidateCtx};

/// The compression transparency check.
pub struct Compress;

/// Base flags applied to every cell; the compress args are appended per cell.
const BASE_FLAGS: &[&str] = &["-rlptgoD", "--numeric-ids"];

/// The compress argument sets to exercise: default codec/level, then level 9.
const COMPRESS_ARGS: [&[&str]; 2] = [&["-z"], &["-z", "--compress-level=9"]];

impl Check for Compress {
    fn name(&self) -> &'static str {
        "compress"
    }

    fn run(&self, ctx: &ValidateCtx) -> Vec<CheckOutcome> {
        let root = ctx.work.join("compress");
        let src = root.join("src");
        if let Err(e) = build_fixture(&src) {
            return vec![CheckOutcome::skip(self.name(), "fixture", e)];
        }
        let expected = support::entry_count(&src);

        let mut outcomes = Vec::new();
        for &transport in ctx.transports {
            for (n, args) in COMPRESS_ARGS.iter().enumerate() {
                outcomes.push(self.cell(ctx, transport, &root, args, n, expected));
            }
        }
        outcomes
    }
}

impl Compress {
    /// Run one `(transport, compress-args)` cell.
    fn cell(
        &self,
        ctx: &ValidateCtx,
        transport: Transport,
        root: &Path,
        compress_args: &[&str],
        n: usize,
        expected: usize,
    ) -> CheckOutcome {
        let cell = format!("{} {}", transport.label(), level_tag(compress_args));
        if transport.needs_ssh() && !support::ssh_ready() {
            return CheckOutcome::skip(self.name(), cell, "no sshd on localhost:22");
        }
        let label = transport.label();
        let src = root.join("src");
        let src = src.as_path();
        let oc_dst = root.join(format!("oc-{label}-{n}"));
        let up_dst = root.join(format!("up-{label}-{n}"));
        let flags: Vec<String> = BASE_FLAGS
            .iter()
            .chain(compress_args.iter())
            .map(|s| s.to_string())
            .collect();

        match pull_into(
            transport.for_upstream(),
            ctx.upstream,
            ctx.upstream,
            src,
            &up_dst,
            &flags,
            ctx.work,
        ) {
            Ok(out) if out.status.success() => {}
            other => return skip_or_fail(self.name(), &cell, "upstream", other),
        }
        match pull_into(
            transport,
            ctx.oc,
            ctx.upstream,
            src,
            &oc_dst,
            &flags,
            ctx.work,
        ) {
            Ok(out) if out.status.success() => {}
            other => return skip_or_fail(self.name(), &cell, "oc", other),
        }

        // Genuine-result guard: both trees must be fully populated.
        if support::entry_count(&up_dst) != expected || support::entry_count(&oc_dst) != expected {
            return CheckOutcome::fail(self.name(), cell, "destination entry count != source");
        }
        // Transparency: identical to upstream's receive and to the source bytes.
        if let Some(diff) = support::content_diff(&oc_dst, &up_dst) {
            return CheckOutcome::fail(self.name(), cell, diff);
        }
        if let Some(diff) = support::content_diff(&oc_dst, src) {
            return CheckOutcome::fail(self.name(), cell, diff);
        }
        if ctx.verbose {
            eprintln!(
                "[compress] {cell}: {} bytes across {expected} entries",
                tree_bytes(&oc_dst)
            );
        }
        CheckOutcome::pass(self.name(), cell)
    }
}

/// Short cell suffix for a compress-arg set, e.g. `-z` or `-z9`.
fn level_tag(compress_args: &[&str]) -> String {
    match compress_args
        .iter()
        .find_map(|a| a.strip_prefix("--compress-level="))
    {
        Some(level) => format!("-z{level}"),
        None => "-z".to_string(),
    }
}

/// Total bytes of regular files under `root` (for the verbose size note).
fn tree_bytes(root: &Path) -> u64 {
    support::rel_entries(root)
        .iter()
        .filter_map(|rel| root.join(rel).symlink_metadata().ok())
        .filter(|m| m.is_file())
        .map(|m| m.len())
        .sum()
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

/// Build the compression fixture. Idempotent: removes any prior tree first.
///
/// Contents span the compressor's range: a highly compressible file (a repeating
/// text pattern), a plain text file, an already-incompressible blob (bytes are
/// derived deterministically from their index - never system randomness - so the
/// fixture is reproducible), and a file in a subdirectory.
fn build_fixture(src: &Path) -> Result<(), String> {
    if src.exists() {
        std::fs::remove_dir_all(src).map_err(|e| e.to_string())?;
    }
    let sub = src.join("sub");
    std::fs::create_dir_all(&sub).map_err(|e| e.to_string())?;

    std::fs::write(src.join("compressible.txt"), repeating(200 * 1024))
        .map_err(|e| e.to_string())?;
    std::fs::write(
        src.join("text.txt"),
        b"The quick brown fox jumps over the lazy dog.\n".repeat(64),
    )
    .map_err(|e| e.to_string())?;
    std::fs::write(src.join("incompressible.bin"), pseudo_random(64 * 1024))
        .map_err(|e| e.to_string())?;
    std::fs::write(sub.join("nested.txt"), b"nested payload under a subdir\n")
        .map_err(|e| e.to_string())?;

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

/// `len` bytes of a short repeating pattern - trivially compressible.
fn repeating(len: usize) -> Vec<u8> {
    const PATTERN: &[u8] = b"oc-rsync compression fidelity harness pattern 0123456789\n";
    PATTERN.iter().copied().cycle().take(len).collect()
}

/// `len` deterministic high-entropy bytes derived from a per-byte index mix.
///
/// A pure function of the index (an integer hash), so the "incompressible"
/// fixture is byte-for-byte reproducible without touching system randomness.
fn pseudo_random(len: usize) -> Vec<u8> {
    (0..len as u64)
        .map(|i| {
            let mut x = i.wrapping_add(0x9e37_79b9_7f4a_7c15);
            x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
            ((x ^ (x >> 31)) & 0xff) as u8
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{level_tag, pseudo_random, repeating, tree_bytes};
    use std::fs;

    #[test]
    fn level_tag_distinguishes_default_from_explicit_level() {
        assert_eq!(level_tag(&["-z"]), "-z");
        assert_eq!(level_tag(&["-z", "--compress-level=9"]), "-z9");
    }

    #[test]
    fn pseudo_random_is_deterministic_and_high_entropy() {
        // Reproducible: same index stream, same bytes - no system randomness.
        assert_eq!(pseudo_random(4096), pseudo_random(4096));
        // Spread across byte values, so it resists compression (unlike a run).
        let bytes = pseudo_random(8192);
        let distinct = bytes
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len();
        assert!(distinct > 200, "only {distinct} distinct byte values");
    }

    #[test]
    fn repeating_fills_exactly_and_cycles_the_pattern() {
        let out = repeating(100);
        assert_eq!(out.len(), 100);
        assert_eq!(out[0], out[57]); // pattern length is 57 bytes
    }

    #[test]
    fn tree_bytes_sums_regular_file_lengths_only() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("d")).unwrap();
        fs::write(dir.path().join("a"), b"12345").unwrap();
        fs::write(dir.path().join("d/b"), b"678").unwrap();
        assert_eq!(tree_bytes(dir.path()), 8);
    }
}
