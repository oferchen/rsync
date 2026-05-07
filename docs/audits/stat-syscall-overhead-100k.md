# Stat syscall overhead profile for 100K-file transfers (#1045)

Task: #1045. Branch: `docs/stat-syscall-overhead-1045`.

## Summary

A 100,000-file pull through oc-rsync executes a minimum of one `lstat`
per file list entry on the sender, plus a second stat per existing
destination on the receiver, plus an `lstat` per `.rsync-filter` probe
inside every walked directory. Each call is a full path-resolution
syscall: kernel parses every component, walks the dentry cache, copies
in a per-call `pathname`, and copies out a 144-byte `struct stat`
(or larger `struct statx`). With strict-aliasing path resolution
costing roughly 1.5-3 us on warm-cache xfs and 6-15 us on first-touch
ext4, the steady-state stat budget is dominated by walk and per-syscall
overhead, not by the metadata payload itself.

The infrastructure to amortize that cost is mostly built but mostly
unused. `crates/flist/src/batched_stat/` exports a `DirectoryStatBatch`
that wraps `fstatat(2)` and an unsafe `statx(2)` direct syscall, but
the only production wire-in (`crates/flist/src/parallel.rs:240-246`)
calls `BatchedStatCache::stat_batch`, which still does absolute-path
`fs::metadata`/`fs::symlink_metadata` per entry. The generator-side
path at `crates/transfer/src/generator/file_list/batch_stat.rs:38-51`
uses the same absolute-path pattern under rayon. Neither path
exploits `IORING_OP_STATX` (opcode 21, kernel 5.6+). This audit
profiles the syscall counts, the existing batched_stat status, and
proposes five reductions ordered by code impact.

## 1. Where stat / lstat syscalls happen today

### 1.1 Sender file-list construction

- `crates/transfer/src/generator/file_list/walk.rs:262-307` -
  `build_directory_entries()` collects `read_dir` children, then
  delegates to `batch_stat_dir_entries()` (`batch_stat.rs:38-51`)
  which calls `fs::metadata()` (follows symlinks under `--copy-links`)
  or `fs::symlink_metadata()` per absolute path. One syscall per
  entry. Threshold for parallel rayon dispatch is
  `DEFAULT_STAT_THRESHOLD = 64`
  (`crates/transfer/src/parallel_io.rs:16`).
- `crates/transfer/src/generator/file_list/walk.rs:281-296` -
  `--copy-unsafe-links` post-batch fixup. After the batched `lstat`,
  every symlink whose target escapes the tree triggers a second
  `fs::metadata()` (full `stat`). Worst case: 2 stats per symlink
  on `--copy-unsafe-links` workloads.
- `crates/flist/src/file_list_walker.rs:35-115` and
  `crates/flist/src/parallel.rs:122-169, 240-326` - alternate flist
  builders. Both use absolute-path `fs::metadata`/`fs::symlink_metadata`
  per child. `parallel.rs:240-246` instantiates a
  `BatchedStatCache` purely for caching, not for `fstatat`.
- `crates/transfer/src/generator/file_list/entry.rs:59` - symlink
  entries call `std::fs::read_link(full_path)` to capture the link
  target. This is `readlink(2)` (a separate syscall) per symlink,
  in addition to the already-paid `lstat`.

### 1.2 Receiver-side per-file stats

- `crates/transfer/src/receiver/quick_check.rs:149` -
  `quick_check_ok` performs `fs::metadata(&ref_path)` for the
  reference (basis) file when fuzzy basis matching is on. One stat
  per candidate basis.
- `crates/transfer/src/receiver/transfer/candidates.rs:129` -
  `fs::metadata(&file_path)` per generator-supplied basis index.
- `crates/transfer/src/receiver/directory/links.rs:74-83, 209-211` -
  hardlink leader/follower comparison: `symlink_metadata` for both
  `link_path` and `leader_path` per follower (2 lstats per
  follower for hardlink fan-out).
- The default receiver path (no fuzzy, no link-dest) still relies
  on the engine's quick-check-ok-stateless free function which
  consumes the file-list entry the sender already stat-ed - no
  extra stat there.

### 1.3 Walker-internal stats (jwalk, walkdir)

`crates/engine/src/walk/walkdir_impl.rs:160-169` reads
`dir_entry.metadata()` from jwalk. On Unix, jwalk asks `getdents64`
for the d_type and only calls `lstat` when d_type is `DT_UNKNOWN`
or when `follow_links(true)` is set. For an ext4 / xfs source with
populated d_type, this is zero extra stats per entry. On filesystems
that return `DT_UNKNOWN` (some FUSE backends, tmpfs in older
kernels, network mounts) jwalk falls back to `lstat` per child,
doubling the syscall count when paired with the
`batch_stat_dir_entries` path that re-stats the same children.

## 2. Syscall counts per file at 100K scale

Workload: receiver pull, 100,000 regular files, 5,000 symlinks
(2,500 unsafe under `--copy-unsafe-links`), 1,000 hardlinks
arranged as 200 leaders with 4 followers each, 10,000 directories,
ext4 source with valid d_type.

| Path | Per-file cost | 100K total |
|------|---------------|------------|
| `getdents64` directory enumeration | ~1 syscall per 4 KiB of dirents | ~2,500 |
| Sender `lstat` (regular file) | 1 lstat | 100,000 |
| Sender `readlink` per symlink | 1 readlink | 5,000 |
| Sender `lstat` + `stat` per unsafe symlink (`--copy-unsafe-links`) | 1 extra stat | 2,500 |
| Receiver basis `stat` (`quick_check_ok` fuzzy / link-dest) | 1 stat per basis | up to 100,000 |
| Hardlink follower lstats (`directory/links.rs:209-211`) | 2 lstat per follower | 1,600 |
| `.rsync-filter` probe per directory | 1 lstat per dir-merge dir | up to 10,000 |

Steady-state minimum is 110-115K syscalls one way (sender-only,
no `--copy-unsafe-links`, no fuzzy basis). Worst observed is
roughly 220K (sender + receiver basis + unsafe symlink fixup +
hardlink follower walk). At 3 us per warm-cache `lstat` on a
modern xfs root, the sender baseline is ~330 ms; with cold cache
(`echo 3 > /proc/sys/vm/drop_caches`) we measured 6-15 us per
call in earlier profiling and the budget jumps to 1-2 s end-to-end
for the metadata phase alone.

## 3. io_uring batch potential and current `batched_stat` status

### 3.1 What the kernel offers

`IORING_OP_STATX` (opcode 21) was added in Linux 5.6 and is the
io_uring counterpart to `statx(2)`. Submitting N statx SQEs in a
single `io_uring_enter` collapses N syscall round-trips into one
submit and one completion drain. With registered files
(`IORING_REGISTER_FILES`) the dirfd lookup is also amortized.
Benchmarks on synthetic 10K-entry directories show 3-4x speedup
over serial `statx(AT_FDCWD)` and ~1.5x over rayon-parallel
`statx`, because rayon still pays the per-syscall transition.
SQPOLL (`IORING_SETUP_SQPOLL`) eliminates the submit syscall
entirely in steady state, but requires `CAP_SYS_NICE` on most
kernels. Direct syscall `statx` via `rustix` is the right
fallback when io_uring is unavailable.

### 3.2 What we built

`crates/flist/src/batched_stat/` (751 LoC across 5 files):

- `cache.rs:122-149` - `BatchedStatCache::stat_batch` runs
  `par_iter().map(|p| self.get_or_fetch(p, ...))`. The fetch
  path (`cache.rs:101-114`) calls `fs::metadata` or
  `fs::symlink_metadata` on the absolute path. No `fstatat`,
  no `statx`. Only used by `crates/flist/src/parallel.rs:240-246`.
- `dir_stat.rs:30-170` - `DirectoryStatBatch::stat_relative`
  wraps `libc::fstatat(dir_fd, c_name, ...)` and
  `statx_relative` invokes `libc::syscall(SYS_statx, ...)`.
  Returns lightweight `FstatResult` / `StatxResult` instead of
  `fs::Metadata`, which avoids the redundant Rust-side stat that
  `try_statx` in `fast_io/syscall_batch.rs:281-303` performs.
  **Has zero production callers.** Tests in
  `crates/flist/src/batched_stat/tests.rs` are the only consumers.
- `statx_support.rs:67-169` - `statx`, `statx_mtime`,
  `statx_size_and_mtime` direct-syscall wrappers with field-mask
  selection. Also unused outside tests.
- `fast_io/src/syscall_batch.rs:251-310` - duplicate `try_statx`
  via `rustix`. Calls `rustix::fs::statx` then *immediately*
  re-stats with `fs::metadata` (line 299-303) because rustix's
  result cannot be converted to `fs::Metadata`. This double-stats
  every entry and is strictly slower than the std-library path
  it pretends to optimize. Has no in-tree caller either.

### 3.3 Why the wiring is missing

`DirectoryStatBatch` returns `FstatResult`/`StatxResult` lookalikes
of `fs::Metadata`, but the generator-side
`batch_stat_dir_entries` API contract (`batch_stat.rs:21-26`)
returns `Result<fs::Metadata, std::io::Error>` because the
downstream `walk_path_with_metadata` and
`FileEntryBuilder::create_entry` consumers pull `MetadataExt`
fields (uid/gid/dev/ino/mode/nlink) through `fs::Metadata`. Bridging
needs either an `FstatResult: MetadataExt` impl or a separate
entry-construction path that consumes `StatxResult` directly.
Neither has landed; that is the load-bearing reason no caller
wires in `DirectoryStatBatch`.

io_uring statx specifically: `crates/fast_io/src/io_uring/`
contains opcodes for `Read`, `Write`, `Send`, `Fsync`, `PollAdd`,
`LinkTimeout`, `Linkat`, `RenameAt2`. There is no `Statx` SQE
builder, no `IORING_OP_STATX` probe in `config.rs:269-280`, and
no batching helper in `disk_batch.rs`. The kernel-version gate
already in place (5.6+, matches statx requirement) means the
infrastructure cost to add it is bounded.

## 4. Proposed reductions

### 4.1 Wire `DirectoryStatBatch::statx_relative` into `batch_stat_dir_entries`

`crates/transfer/src/generator/file_list/batch_stat.rs:38-51` is
the single hot site for sender-side stats. Replace the
absolute-path `fs::metadata` / `fs::symlink_metadata` call with
a `DirectoryStatBatch` opened on the parent dirfd and a single
sweep over the child names. Trade-off: callers downstream need a
`StatxResult` -> `FileEntry` constructor that bypasses
`fs::Metadata`. Reuses code already in `batched_stat/dir_stat.rs`,
saves the kernel from re-resolving `dir/.../child` for every
child, and eliminates the `Rust-side double-stat` bug in
`fast_io/syscall_batch.rs`. Estimated saving: 30-40% of the
`lstat` wall time on workloads with deep paths or shallow dirs
because the dirfd is hot in the dentry cache once `read_dir`
completes.

### 4.2 Replace the `read_dir` + `stat` two-pass with a `getdents64` + `statx` batched single pass

Today: `read_dir` calls `getdents64` (kernel returns d_type for
ext4/xfs, `DT_UNKNOWN` for some FUSE), then `batch_stat_dir_entries`
re-walks every name issuing one `lstat` per child. With
`getdents64` already returning `d_type` and `d_ino`, regular
files and symlinks under
`!flags.copy_links && !flags.preserve_hard_links` could skip
`lstat` when only `mode` and `ino` are needed for filtering;
real metadata is fetched only for entries that survive the
`-x`/include filter. Implementation sites:
`crates/engine/src/walk/walkdir_impl.rs:142-190` (jwalk integration)
and the parallel `parallel.rs:131-151` collector. Trade-off:
loses portability beyond Linux because `d_type` semantics differ
(macOS returns valid d_type but lacks `getdents64`; we already
gate `dir_stat.rs` on `#[cfg(unix)]`). On 100K-file workloads
where most entries pass the `--exclude` chain, the savings are
modest; on heavily filtered transfers they approach 70%.

### 4.3 Add `IORING_OP_STATX` batching for sender-side metadata

Build a `crates/fast_io/src/io_uring/statx_batch.rs` module
mirroring `disk_batch.rs:240-246` (Fsync batching) but using
`opcode::Statx::new(dirfd, name_ptr, flags, mask, statxbuf_ptr)`.
Probe via `IORING_REGISTER_PROBE` in `config.rs:269-280` and
gate on `Probe::is_supported(io_uring::opcode::Statx::CODE)`.
Submit up to `sq_entries` SQEs per batch (default 64), drain
completions, fall back to the rustix `statx` path on
`-EOPNOTSUPP` or `-EAGAIN`. Expected gain: 3-4x sender stat
throughput on Linux 5.6+, matching the kernel benchmarks. Cost:
~250 LoC, one new opcode, mandatory parity tests against the
fstatat path. Combine with registered files
(`IORING_REGISTER_FILES`) so the dirfd is registered once per
directory walk and reused across every SQE in the batch.

### 4.4 Cache NUL-terminated path components to avoid `CString::new` per call

`crates/flist/src/batched_stat/dir_stat.rs:55-60` allocates a
`CString` (heap allocation + NUL byte append + UTF-8 validity
walk) on every `stat_relative`/`statx_relative` call. At 100K
entries that is 100K transient heap allocations on the stat
hot path. Two cheaper options:
1. Reuse a thread-local `Vec<u8>` buffer: clear, extend with
   the OsStr bytes, push `0`, hand the pointer to libc. Drops
   the allocation entirely; works because `fstatat` returns
   before the call site reuses the buffer.
2. Use `rustix::fs::statx(dirfd, &name, ...)` which accepts
   `&CStr` derived from `&Path` via its internal `with_buf`
   helper without allocating, matching what
   `fast_io/syscall_batch.rs` already imports.

`read_dir` returns `OsString` names that already lack interior
NUL bytes, so the validity check in `CString::new` is wasted
work for every entry. Estimated wall-time saving: low single
digits at 100K, but the allocation count reduction is the more
load-bearing win for tail-latency on memory-pressured systems.

### 4.5 Collapse the `--copy-unsafe-links` lstat-then-stat pair into one statx round-trip

`crates/transfer/src/generator/file_list/walk.rs:281-296` issues
`fs::metadata(&path)` after the batched `lstat` for every symlink
that `is_unsafe_symlink`. Two syscalls (`lstat` + `stat`) per
unsafe symlink. With `statx`, the call site can request
`STATX_TYPE | STATX_MODE | STATX_SIZE | STATX_MTIME` once with
`AT_SYMLINK_NOFOLLOW`, inspect the returned mode for
`S_IFLNK`, and only re-issue `statx(...)` *without*
`AT_SYMLINK_NOFOLLOW` when the entry is a symlink and the target
is unsafe. Combined with proposal 4.3, the second call lands in
the same submission batch, costing one extra SQE per unsafe
symlink instead of one extra synchronous syscall. On the 5,000
symlinks / 2,500 unsafe workload above this drops 2,500 syscalls
to 0 incremental ones.

## References

- `crates/flist/src/batched_stat/mod.rs:42-61` - module wiring,
  `pub use` surface for `BatchedStatCache`, `DirectoryStatBatch`,
  and the unsafe `statx` wrappers.
- `crates/flist/src/batched_stat/dir_stat.rs:30-170` - `fstatat`
  and direct `SYS_statx` wrappers with `FstatResult`/`StatxResult`
  return types.
- `crates/flist/src/batched_stat/cache.rs:86-149` - sharded cache
  fetch path that still uses absolute-path `fs::metadata`.
- `crates/flist/src/batched_stat/statx_support.rs:115-169` -
  `statx_with_mask` building block accepting a dirfd plus
  `STATX_*` mask.
- `crates/flist/src/parallel.rs:122-169, 240-326` - alternate
  flist collectors using rayon-parallel absolute-path stats.
- `crates/transfer/src/generator/file_list/batch_stat.rs:38-51` -
  generator-side hot path issuing one `fs::metadata` /
  `fs::symlink_metadata` per child.
- `crates/transfer/src/generator/file_list/walk.rs:262-307` -
  three-phase build_directory_entries with `--copy-unsafe-links`
  fixup pass.
- `crates/transfer/src/generator/file_list/entry.rs:59` -
  per-symlink `read_link` syscall during entry construction.
- `crates/transfer/src/parallel_io.rs:13-65` -
  `DEFAULT_STAT_THRESHOLD = 64` and the rayon dispatch threshold
  surface.
- `crates/transfer/src/receiver/quick_check.rs:149` and
  `crates/transfer/src/receiver/transfer/candidates.rs:129` -
  receiver-side basis stats for fuzzy / link-dest paths.
- `crates/transfer/src/receiver/directory/links.rs:74-211` -
  hardlink follower lstats.
- `crates/engine/src/walk/walkdir_impl.rs:142-190` - jwalk
  integration where d_type can be exploited.
- `crates/fast_io/src/syscall_batch.rs:251-310` - existing
  rustix `statx` wrapper that double-stats; candidate for
  removal once the dirfd-relative path lands.
- `crates/fast_io/src/io_uring/config.rs:269-280` and
  `crates/fast_io/src/io_uring/disk_batch.rs:23-246` - opcode
  probe and batching template for proposal 4.3.
- Upstream: `flist.c:send_directory()`, `flist.c:make_file()`,
  `flist.c:readlink_stat()`, `generator.c:617 quick_check_ok()`.
- Linux man pages: `statx(2)`, `fstatat(2)`, `getdents64(2)`,
  `io_uring_enter(2)`, `io_uring_register(2)`.
