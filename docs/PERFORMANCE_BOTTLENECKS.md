# Performance Bottlenecks Analysis

This document tracks identified performance bottlenecks in the rsync codebase with their locations, severity, and recommended fixes.

## Benchmark Infrastructure

### Running Benchmarks

```bash
# Quick profiling script (compares oc-rsync vs upstream)
./scripts/profile_transfer.sh

# With perf profiling
./scripts/profile_transfer.sh --perf

# With flamegraph generation
./scripts/profile_transfer.sh --flamegraph

# Criterion benchmarks for real rsync:// transfers
cargo bench -p core --bench transfer_benchmark
```

## High Priority Bottlenecks

### 1. Delta Token Allocation (HOT PATH)

**Location:** `crates/transfer/src/delta_apply.rs`
- Line 353: `let mut data = vec![0u8; token as usize];`
- Line 326: `let mut block_data = vec![0u8; bytes_to_copy];`
- Line 375: `let mut expected = vec![0u8; expected_len];`

**Problem:** Each token in the delta application loop allocates a new vector. For files with many small blocks, this causes repeated allocations in the hot path.

**Impact:** Significant on large files with thousands of delta tokens.

**Fix:** Add a reusable buffer pool to `DeltaApplicator`, reuse buffer across token calls.

---

### 2. PathBuf Cloning in File Walker (HOT PATH)

**Location:** `crates/flist/src/file_list_walker.rs`
- Line 155: `let mut rel = state.relative_prefix.clone();`
- Line 95: `next_state = Some((full_path.clone(), relative_path.clone(), depth));`
- Line 101: `next_state = Some((canonical, relative_path.clone(), depth));`

**Problem:** DirectoryState clones PathBuf repeatedly in the traversal loop, once for every filesystem entry.

**Impact:** For large directory trees (millions of files), this adds up to millions of allocations.

**Fix:** Use `Cow<Path>` or references where possible; build paths once in a local buffer.

---

### 3. OsString Cloning in Directory Entry Iteration

**Location:** `crates/flist/src/file_list_walker.rs:216`
- `Some(name.clone())`

**Problem:** Every filename is cloned when retrieved from the entries vector.

**Fix:** Return `&OsString` and store index, or use `std::mem::take()`.

---

## Medium Priority Bottlenecks

### 4. Per-Message Payload Buffer Allocation

**Location:** `crates/protocol/src/multiplex/helpers.rs:77-80`
```rust
pub(super) fn read_payload<R: Read>(reader: &mut R, len: usize) -> io::Result<Vec<u8>> {
    let mut payload = Vec::new();  // No capacity hint!
    read_payload_into(reader, &mut payload, len)?;
    Ok(payload)
}
```

**Problem:** No capacity pre-allocation; creates new vector for each message frame.

**Fix:** Add `Vec::with_capacity(len)` or use reusable buffer API.

---

### 5. Vectored I/O Fallback Overhead

**Location:** `crates/protocol/src/multiplex/io.rs:92-189`

**Problem:** Complex branching logic on every write; checks vectored I/O support per-message.

**Fix:** Cache vectored I/O capability at socket level; check once at initialization.

---

### 6. Sequential Path Enumeration in Parallel Module

**Location:** `crates/flist/src/parallel.rs:116`
- `let paths = collect_paths_recursive(&root, &root, follow_symlinks);`

**Problem:** Directory enumeration is sequential; only metadata fetching is parallelized.

**Fix:** Parallelize directory recursion using rayon's `par_bridge()` or work-stealing queue.

---

### 7. Repeated canonicalize() Calls

**Location:** `crates/flist/src/file_list_walker.rs:69`
- `let canonical = fs::canonicalize(&fs_path)...`

**Problem:** `canonicalize()` involves multiple syscalls; happens even for non-symlink directories.

**Fix:** Cache using inode+device; skip canonicalize for non-symlinks.

---

## Low Priority Bottlenecks

### 8. Buffer Per-File Allocation Without Feature Flag

**Location:** `crates/engine/src/local_copy/executor/file/copy/transfer.rs:291`

**Problem:** Without `optimized-buffers` feature, allocates new buffer for every file.

**Fix:** Always use BufferPool architecture.

---

### 9. Error Path PathBuf Cloning

**Location:** `crates/flist/src/file_list_walker.rs:31,54,70,100`

**Problem:** Cloning PathBuf for error context.

**Fix:** Use `&Path` in error structs; convert to owned only when formatting.

---

## Summary Table

| # | Issue | Severity | File | Line(s) |
|---|-------|----------|------|---------|
| 1 | Delta token allocation | **HIGH** | delta_apply.rs | 326, 353, 375 |
| 2 | PathBuf cloning in walker | **HIGH** | file_list_walker.rs | 95, 101, 155 |
| 3 | OsString cloning | **MEDIUM** | file_list_walker.rs | 216 |
| 4 | Payload buffer allocation | **MEDIUM** | helpers.rs | 77-80 |
| 5 | Vectored I/O branching | **MEDIUM** | io.rs | 92-189 |
| 6 | Sequential enumeration | **MEDIUM** | parallel.rs | 116 |
| 7 | Repeated canonicalize | **MEDIUM** | file_list_walker.rs | 69 |
| 8 | Buffer per-file | **LOW** | transfer.rs | 291 |
| 9 | Error path cloning | **LOW** | file_list_walker.rs | 31, 54, 70 |

## Expected Impact

Fixing high and medium priority issues could result in:
- 10-30% throughput improvement on typical workloads
- Significant memory allocation reduction
- Better cache locality in hot paths
- Improved parallelism for large directory trees
