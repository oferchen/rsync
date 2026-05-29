//! Heap allocation profiler for the oc-rsync resident file list.
//!
//! Builds a `Vec<FileEntry>` from a real directory tree using the same
//! production constructors and path interner that the wire-decode path
//! (`FileListReader` / `from_raw_bytes` + `set_dirname`) uses on the
//! receiver side. The whole flist is held resident, which is exactly the
//! allocation set that determines peak RSS for large transfers.
//!
//! The profiled region is split into phases so the dhat `HeapStats` deltas
//! attribute resident bytes to the individual `FileEntry` fields:
//!
//! - `Vec<FileEntry>` backing buffer (the flat array of inline structs),
//! - interned `dirname` `Arc<Path>` allocations plus the interner `HashMap`,
//! - per-entry `name` `PathBuf` allocations,
//! - boxed `extras` (zero for plain regular files, the common case).
//!
//! # Usage
//!
//! ```bash
//! cargo build --profile dhat -p dhat-profile
//! target/dhat/dhat-profile <fixture_dir>
//! ```
//!
//! The phase deltas and final dhat summary (peak heap, total bytes,
//! allocation count) are printed on exit.

use std::env;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use protocol::flist::{FileEntry, FileFlags, PathInterner};

#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

/// Recursively collects every regular file path under `root`.
fn collect_files(root: &Path, out: &mut Vec<PathBuf>) {
    let entries = match std::fs::read_dir(root) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("read_dir {} failed: {e}", root.display());
            return;
        }
    };
    for entry in entries.flatten() {
        let path = entry.path();
        match entry.file_type() {
            Ok(ft) if ft.is_dir() => collect_files(&path, out),
            Ok(ft) if ft.is_file() => out.push(path),
            _ => {}
        }
    }
}

/// Snapshots the live dhat heap and prints a labelled line plus the byte/block
/// delta against the previous snapshot.
fn snapshot(label: &str, prev_bytes: &mut u64, prev_blocks: &mut u64) {
    let s = dhat::HeapStats::get();
    let db = s.curr_bytes as i64 - *prev_bytes as i64;
    let dn = s.curr_blocks as i64 - *prev_blocks as i64;
    eprintln!(
        "phase {label}: curr_bytes={} curr_blocks={} (delta_bytes={} delta_blocks={})",
        s.curr_bytes, s.curr_blocks, db, dn
    );
    *prev_bytes = s.curr_bytes;
    *prev_blocks = s.curr_blocks;
}

fn main() {
    let fixture = env::args()
        .nth(1)
        .expect("usage: dhat-profile <fixture_dir>");
    let root = PathBuf::from(&fixture);

    // Phase 0: scan the tree (paths only), outside the profiled region so the
    // dhat profile is dominated by the resident flist, not the directory walk.
    let mut abs_paths = Vec::new();
    collect_files(&root, &mut abs_paths);
    let file_count = abs_paths.len();
    eprintln!("scanned {file_count} files under {fixture}");

    // Pre-compute relative path bytes and mtimes outside the profiled region
    // (the source filesystem read is not part of the resident flist cost).
    let mut rels: Vec<(Vec<u8>, i64)> = Vec::with_capacity(file_count);
    for abs in &abs_paths {
        let rel = abs.strip_prefix(&root).unwrap_or(abs);
        let mtime = std::fs::metadata(abs)
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_secs() as i64);
        rels.push((rel.as_os_str().as_bytes().to_vec(), mtime));
    }
    drop(abs_paths);

    // Profiled region of interest: build the resident flist.
    // testing() mode enables HeapStats::get() to return live statistics.
    let _profiler = dhat::Profiler::builder().testing().build();
    let mut pb = 0u64;
    let mut pn = 0u64;
    snapshot("0_baseline", &mut pb, &mut pn);

    // Phase 1: reserve the Vec<FileEntry> backing buffer. This is the single
    // flat allocation that holds every inline FileEntry struct.
    let mut flist: Vec<FileEntry> = Vec::with_capacity(file_count);
    snapshot("1_vec_backing", &mut pb, &mut pn);

    // Phase 2: intern every parent directory. This allocates the unique
    // dirname Arc<Path> values plus the interner HashMap backing - the shared
    // dirname cost amortized across all entries in the same directory.
    let mut interner = PathInterner::new();
    let mut dirnames: Vec<std::sync::Arc<Path>> = Vec::with_capacity(file_count);
    for (bytes, _) in &rels {
        let p = Path::new(std::ffi::OsStr::from_bytes(bytes));
        let dir = p.parent().unwrap_or_else(|| Path::new(""));
        dirnames.push(interner.intern(dir));
    }
    snapshot("2_dirname_intern", &mut pb, &mut pn);

    // Phase 3: build every FileEntry. from_raw_bytes allocates the per-entry
    // name PathBuf; set_dirname attaches the already-interned Arc (no new
    // allocation). Plain regular files leave extras = None (no Box).
    for (i, (bytes, mtime)) in rels.iter().enumerate() {
        let mut entry =
            FileEntry::from_raw_bytes(bytes.clone(), 0, 0o100_644, *mtime, 0, FileFlags::default());
        entry.set_dirname(std::sync::Arc::clone(&dirnames[i]));
        // Archive mode (-a) populates ownership; matches the realistic case.
        entry.set_uid(1000);
        entry.set_gid(1000);
        flist.push(entry);
    }
    snapshot("3_entry_names", &mut pb, &mut pn);

    eprintln!(
        "resident flist: {} entries, {} unique dirnames interned",
        flist.len(),
        interner.len()
    );

    // Final peak/total summary while the whole flist is resident.
    let stats = dhat::HeapStats::get();
    eprintln!(
        "dhat heap stats: curr_blocks={} curr_bytes={} max_blocks={} max_bytes={} total_blocks={} total_bytes={}",
        stats.curr_blocks,
        stats.curr_bytes,
        stats.max_blocks,
        stats.max_bytes,
        stats.total_blocks,
        stats.total_bytes,
    );

    // Touch the data so the optimizer cannot elide the flist.
    let total: u64 = flist.iter().map(|e| e.size()).sum();
    eprintln!("checksum (total bytes across entries): {total}");

    drop(flist);
    drop(dirnames);
    drop(interner);
}
