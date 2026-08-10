# Batched Metadata Syscalls

This document describes the batched metadata syscall optimization implemented in the `flist` crate.

## Overview

Directory traversal in rsync requires fetching metadata (size, permissions, timestamps, etc.) for every file encountered. On traditional implementations, this requires one `stat()` or `lstat()` syscall per file, which becomes a major performance bottleneck for large directory trees.

This implementation provides several optimizations to reduce syscall overhead:

1. **Parallel stat operations** using rayon to saturate I/O bandwidth
2. **Metadata caching** to avoid redundant stats of the same path
3. **Directory-relative operations** using `fstatat()` to reduce path resolution
4. **Modern syscalls** using `statx()` on Linux 4.11+ for better performance

## Performance

On large directory trees (>10,000 files), batched metadata fetching provides **2-4x speedup** compared to sequential stat operations, especially on:

- Network filesystems (NFS, CIFS, SMB)
- SSDs with high IOPS capabilities
- Multi-core systems
- Systems with high syscall overhead

## Architecture

### Components

#### 1. `BatchedStatCache`

Thread-safe cache for metadata results. Automatically deduplicates stat operations for paths that are accessed multiple times.

```rust
use flist::BatchedStatCache;
use std::path::Path;

let cache = BatchedStatCache::new();

// First access - performs stat
let metadata1 = cache.get_or_fetch(Path::new("/tmp/file.txt"), false)?;

// Second access - returns cached result
let metadata2 = cache.get_or_fetch(Path::new("/tmp/file.txt"), false)?;
assert!(Arc::ptr_eq(&metadata1, &metadata2));
```

#### 2. `DirectoryStatBatch` (Unix only)

Uses `openat()` + `fstatat()` to stat files relative to a directory file descriptor. This reduces path resolution overhead when statting many files in the same directory.

```rust
use flist::batched_stat::DirectoryStatBatch;
use std::ffi::OsString;

let batch = DirectoryStatBatch::open("/tmp")?;
let metadata = batch.stat_relative(&OsString::from("file.txt"), false)?;
```

#### 3. Parallel Collection with `collect_with_batched_stats()`

Combines path enumeration with parallel metadata fetching:

```rust
use flist::parallel::collect_with_batched_stats;
use std::path::PathBuf;

let entries = collect_with_batched_stats(
    PathBuf::from("/large/directory/tree"),
    false  // don't follow symlinks
)?;
```

### Algorithm

1. **Enumerate paths sequentially**
   - Directory reading must be sequential to maintain correct ordering
   - Only collects paths, no metadata yet
   - Fast operation, minimal syscall overhead

2. **Batch metadata fetching in parallel**
   - Splits path list across rayon thread pool
   - Each thread performs stat operations independently
   - Results are cached to avoid duplicates

3. **Combine and sort results**
   - Merge paths with metadata into `FileListEntry` structures
   - Sort by relative path for deterministic ordering
   - Partition successes and failures

## Implementation Details

### Syscall Optimizations

#### `fstatat()` on Unix

```rust
let dir = fs::File::open("/tmp")?;
let dir_fd = dir.as_raw_fd();

// Stat relative to directory fd
libc::fstatat(
    dir_fd,
    c_name.as_ptr(),
    &mut stat_buf,
    libc::AT_SYMLINK_NOFOLLOW,
);
```

Benefits:
- Reduces path resolution overhead
- More efficient for many files in the same directory
- Atomic relative to the directory (no race conditions)

#### `statx()` on Linux 4.11+

```rust
libc::syscall(
    libc::SYS_statx,
    libc::AT_FDCWD,
    path.as_ptr(),
    flags,
    libc::STATX_BASIC_STATS,
    &mut statx_buf,
);
```

Benefits:
- More efficient than traditional `stat()`
- Can request only needed fields (reduces I/O)
- Better support for extended attributes
- Future-proof for new metadata fields

### Caching Strategy

The `BatchedStatCache` uses `Arc<fs::Metadata>` to enable cheap cloning while maintaining a single source of truth:

```rust
pub struct BatchedStatCache {
    cache: Arc<Mutex<HashMap<PathBuf, Arc<fs::Metadata>>>>,
}
```

This design:
- Allows thread-safe sharing via `Arc<Mutex<_>>`
- Avoids cloning large metadata structures via `Arc<fs::Metadata>`
- Enables reference-counted cleanup when entries are no longer needed

### Parallel Execution

Uses rayon's parallel iterators to maximize CPU utilization:

```rust
paths.par_iter()
    .map(|path| self.get_or_fetch(path, follow_symlinks))
    .collect()
```

Rayon automatically:
- Distributes work across CPU cores
- Balances load dynamically
- Minimizes synchronization overhead

## Usage

### Enable the Feature

Add to your `Cargo.toml`:

```toml
[dependencies]
flist = { version = "0.5", features = ["parallel"] }
```

### Basic Usage

```rust
use flist::parallel::collect_with_batched_stats;
use std::path::PathBuf;

let entries = collect_with_batched_stats(
    PathBuf::from("/path/to/directory"),
    false,  // follow_symlinks
)?;

for entry in entries {
    println!("{}: {} bytes",
        entry.relative_path().display(),
        entry.metadata().len()
    );
}
```

### With Custom Cache

```rust
use flist::{BatchedStatCache, parallel::collect_with_batched_stats};

let cache = BatchedStatCache::with_capacity(10_000);

// Use cache across multiple operations
let entries1 = /* ... */;
let entries2 = /* ... */;

println!("Cache hit rate: {:.1}%",
    100.0 * cache.len() as f64 / (entries1.len() + entries2.len()) as f64
);
```

### Directory-Relative Stats (Unix)

```rust
use flist::batched_stat::DirectoryStatBatch;
use std::ffi::OsString;

let batch = DirectoryStatBatch::open("/tmp")?;

let names = vec![
    OsString::from("file1.txt"),
    OsString::from("file2.txt"),
    OsString::from("file3.txt"),
];

let results = batch.stat_batch_relative(&names, false);
```

## Benchmarking

Run the included benchmark:

```bash
cargo run --release --features parallel --example batched_stat_benchmark -- /usr/share
```

Example output:
```
Benchmarking directory: /usr/share

=== Sequential Traversal ===
Found 45,231 entries
Time: 2.341s

=== Parallel with Batched Stats ===
Found 45,231 entries
Time: 0.623s

=== Results ===
Speedup: 3.76x
Time saved: 1.718s
```

## Platform Support

| Platform | Parallel Stats | `fstatat` | `statx` |
|----------|---------------|-----------|---------|
| Linux    | ✓             | ✓         | ✓ (4.11+) |
| macOS    | ✓             | ✓         | ✗       |
| *BSD     | ✓             | ✓         | ✗       |
| Windows  | ✓             | ✗         | ✗       |

On platforms without `fstatat`/`statx`, the implementation falls back to standard `stat()` calls while still benefiting from parallelization and caching.

## Safety

The implementation uses `unsafe` blocks for syscalls:
- All syscall wrappers check return codes
- Errors are propagated via `io::Result`
- No memory unsafety (proper use of `CString`, `zeroed()`, etc.)
- File descriptors are properly closed via `Drop`

## Future Improvements

1. **io_uring integration** - Batch syscalls at kernel level (Linux 5.6+)
2. **Adaptive batching** - Adjust batch size based on filesystem characteristics
3. **Prefetching** - Predict which paths will be stat'd next
4. **Extended attributes** - Batch `getxattr()` calls alongside metadata
5. **Cross-platform optimization** - Windows equivalent using `NtQueryDirectoryFile`

## References

- Linux `statx()`: https://man7.org/linux/man-pages/man2/statx.2.html
- POSIX `fstatat()`: https://pubs.opengroup.org/onlinepubs/9699919799/functions/fstatat.html
- Rayon parallel iterators: https://docs.rs/rayon/latest/rayon/iter/
