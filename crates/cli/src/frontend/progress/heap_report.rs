//! The `--info=stats3` heap-statistics block.
//!
//! upstream: main.c:484 `show_malloc_stats()`, called from `handle_stats()`
//! (main.c:337-340) under `INFO_GTE(STATS, 3)` with the comment "These come out
//! from every process", ahead of `output_summary()`.
//!
//! # Two deliberate divergences, both forced by oc's architecture
//!
//! 1. **Field names are jemalloc's, not glibc's.** Upstream reads `mallinfo2`;
//!    oc installs jemalloc as the global allocator on every Unix target
//!    (`src/bin/oc-rsync.rs`), so glibc's arena is never allocated from and its
//!    counters would describe a heap oc does not use. The block keeps upstream's
//!    shape - header, then `  name: value (description)` rows - and names the
//!    fields that actually describe oc's heap.
//!
//! 2. **One heap, several role labels.** Upstream forks, so its blocks come from
//!    separate address spaces with genuinely independent arenas. oc's roles are
//!    threads sharing one heap, so the figures are necessarily identical across
//!    roles. The header says so rather than letting a reader infer three heaps
//!    from three blocks.

use std::io::{self, Write};

use core::branding::client_program_name;
use fast_io::heap_stats::{HeapStats, heap_stats};

/// Logical roles a transfer drives, in upstream's process order.
///
/// upstream renders `(%s%s%s)` from `am_server` / `am_daemon` / `who_am_i()`
/// (main.c:490-491); a local transfer yields exactly these three.
const ROLES: [&str; 3] = ["sender", "server receiver", "server generator"];

/// Marks the figures as describing one shared process heap.
///
/// Without it a reader comparing against upstream would read three blocks as
/// three independent arenas - true upstream, false here.
const SHARED_HEAP_NOTE: &str = "[process-wide; oc roles are threads, not processes]";

/// One `name: value (description)` row.
///
/// upstream's `PRINT_ALLOC_NUM(title, descr, num)` (main.c:493-495) pairs each
/// counter with a fixed description; the same pairing keeps the block
/// self-describing.
type Row = (&'static str, fn(&HeapStats) -> u64, &'static str);

/// The counters jemalloc exposes, ordered smallest scope to largest so the block
/// reads as a containment chain: live bytes, then pages, then mappings.
const ROWS: [Row; 6] = [
    ("allocated", |s| s.allocated, "bytes in live allocations"),
    ("active", |s| s.active, "bytes in active pages"),
    ("metadata", |s| s.metadata, "allocator bookkeeping"),
    (
        "resident",
        |s| s.resident,
        "bytes in physically resident pages",
    ),
    ("mapped", |s| s.mapped, "bytes in mapped extents"),
    (
        "retained",
        |s| s.retained,
        "bytes retained, not returned to OS",
    ),
];

/// Writes one heap-statistics block per logical role.
///
/// Samples the allocator once and renders every role from that single sample:
/// the roles share a heap, so a per-role re-sample would imply an independence
/// the process model does not have and would make the blocks differ only by
/// scheduling noise.
///
/// Emits nothing when no introspectable allocator is present, mirroring
/// upstream's `#ifdef MEM_ALLOC_INFO` no-op - `show_malloc_stats()` still runs
/// and prints no block.
pub(crate) fn emit_heap_statistics<W: Write + ?Sized>(stdout: &mut W) -> io::Result<()> {
    let Some(stats) = heap_stats() else {
        return Ok(());
    };
    render_blocks(stdout, &stats, client_program_name(), std::process::id())
}

/// Renders the blocks for one sample. Split from sampling so the wire shape is
/// testable without depending on which allocator the test binary links.
fn render_blocks<W: Write + ?Sized>(
    stdout: &mut W,
    stats: &HeapStats,
    program: &str,
    pid: u32,
) -> io::Result<()> {
    for role in ROLES {
        // upstream: main.c:489 `rprintf(FCLIENT, "\n")` precedes each block.
        writeln!(stdout)?;
        writeln!(
            stdout,
            "{program}[{pid}] ({role}) heap statistics {SHARED_HEAP_NOTE}:"
        )?;
        for (name, read, description) in ROWS {
            let title = format!("{name}:");
            writeln!(
                stdout,
                "  {:<11}{:>10}   ({})",
                title,
                read(stats),
                description
            )?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: HeapStats = HeapStats {
        allocated: 1,
        active: 2,
        metadata: 3,
        resident: 4,
        mapped: 5,
        retained: 6,
    };

    fn rendered() -> String {
        let mut out = Vec::new();
        render_blocks(&mut out, &SAMPLE, "oc-rsync", 4123).expect("render");
        String::from_utf8(out).expect("utf8")
    }

    /// upstream's testsuite cell (`misc-coverage_test.py`) requires at least
    /// three occurrences of the literal `heap statistics` - upstream forks a
    /// sender, a server receiver and a server generator, and each prints one.
    ///
    /// The bound is written as a literal, not as `ROLES.len()`: keying it to the
    /// constant would make the assertion follow any change to the constant and
    /// pin nothing.
    #[test]
    fn emits_the_three_blocks_the_testsuite_requires() {
        assert_eq!(rendered().matches("heap statistics").count(), 3);
    }

    /// Each block names its role, so the three are distinguishable. The role
    /// names are spelled out for the same reason the count is: they mirror
    /// upstream's `am_server` / `am_daemon` / `who_am_i()` rendering.
    #[test]
    fn each_block_is_headed_by_its_role_and_pid() {
        let out = rendered();
        for role in ["sender", "server receiver", "server generator"] {
            assert!(
                out.contains(&format!(
                    "oc-rsync[4123] ({role}) heap statistics {SHARED_HEAP_NOTE}:"
                )),
                "missing header for {role} in:\n{out}"
            );
        }
    }

    /// Pins upstream's row layout: two-space indent, left-padded title to 11,
    /// right-aligned value to 10, then the parenthesised description.
    #[test]
    fn rows_use_the_upstream_column_layout() {
        assert!(
            rendered().contains("  allocated:          1   (bytes in live allocations)"),
            "row layout drifted:\n{}",
            rendered()
        );
    }

    /// Every counter is reported; a field added to `HeapStats` without a row
    /// here would silently go unreported.
    #[test]
    fn every_row_is_present_in_each_block() {
        let out = rendered();
        for (name, _, description) in ROWS {
            assert_eq!(
                out.matches(&format!("{name}:")).count(),
                ROLES.len(),
                "{name} not reported once per role"
            );
            assert_eq!(out.matches(description).count(), ROLES.len());
        }
    }
}
