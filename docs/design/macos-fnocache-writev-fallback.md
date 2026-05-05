# macOS F_NOCACHE and writev fallback (#1657)

Tracking issue: oc-rsync task #1657.

Related design notes and prior evaluations:

- `docs/design/macos-kqueue-fast-io.md` (#1385) - the longer-term
  asynchronous-I/O backend whose readiness model this fallback
  composes with. Section 7.2 of that document names `F_NOCACHE`
  plus `writev` as the complement, not the alternative, and pins
  down the `KqueueConfig::f_nocache` knob.
- `docs/audits/async-file-writer-trait.md` (#1655) - the unified
  `AsyncFileWriter` trait that exposes `WritevAsyncWriter` as one
  of the macOS factory branches.
- `docs/audits/macos-dispatch-io.md` (#1653, completed) - the
  feasibility evaluation that closed `dispatch_io` as not-a-fit.
- `crates/fast_io/src/platform_copy/mod.rs:159-186` -
  `try_clonefile` (#1388). `:188-213` - `try_fcopyfile` (used by
  the cross-platform copy benchmark, #1659).

This document is design-only. No code lands in this PR.

## 1. Motivation

oc-rsync ships three batched-write backends plus one synchronous
fallback: Linux `IoUringDiskBatch`
(`crates/fast_io/src/io_uring/disk_batch.rs:45`), Windows
`IocpDiskBatch` (`crates/fast_io/src/iocp/disk_batch.rs:87`), and
the `Buffered` arm of `disk_commit::writer::Writer`
(`crates/transfer/src/disk_commit/writer.rs:141`) for everything
else (a synchronous `std::fs::File` plus a 256 KB reusable buffer,
`crates/transfer/src/disk_commit/writer.rs:20`).

macOS has no asynchronous disk path. Two efforts close that gap:

1. **Long term, asynchronous.** The kqueue backend
   (`docs/design/macos-kqueue-fast-io.md`, #1385) introduces a
   `KqueueDiskBatch` whose lifecycle parallels io_uring. It is the
   strategic destination but requires a new `crates/fast_io/src/`
   module, a runtime probe, a new `Writer` arm, and a CI matrix
   entry. Section 10 of that design lays out a five-step
   migration; landing it is months, not weeks.
2. **Near term, synchronous but tuned.** This design: open the
   destination fd with `F_NOCACHE` for large transfers, issue
   `writev(2)` for the buffered-flush plus large-chunk pair, leave
   the disk-commit thread untouched. No new module, no completion
   topology, no kqueue probe. The win is measured against
   `BufWriter<File>`, not against io_uring.

The two compose: a kqueue fd opened with `O_NONBLOCK | F_NOCACHE`
still receives readiness events through `kevent`, and the existing
`write_all_vectored`
(`crates/transfer/src/disk_commit/writer.rs:35`) already returns
the right byte count when the kernel issues an unbuffered write.

The cross-platform benchmark from #1659 measured the macOS
`Buffered` path at 5-8% behind upstream rsync 3.4.1 on the
4 KiB-file mix on a Mac mini M2 with NVMe. The dominant costs are
the `memcpy` to the unified buffer cache and the per-flush syscall
count. `F_NOCACHE` removes the first; `writev` collapses the
second.

## 2. F_NOCACHE semantics

`F_NOCACHE` is a per-fd `fcntl(2)` operation that disables caching
of file data on subsequent `read(2)` and `write(2)` calls
(`man 2 fcntl`, `F_NOCACHE`). The flag maps onto `IO_NOCACHE` in
xnu's VFS layer (`bsd/vfs/vfs_syscalls.c`), honoured by HFS+,
APFS, and NFS (advisory there).

When set on the destination fd: `write(2)`, `writev(2)`,
`pwrite(2)`, and `pwritev(2)` return once the data is queued to
the device's writeback path, without populating the unified buffer
cache (UBC). The kernel may still coalesce small writes; it does
not duplicate data into the cache. `fsync(2)` and `F_FULLFSYNC`
semantics are unchanged.

`F_NOCACHE` is a throughput win when the working set exceeds the
UBC and a latency loss when it fits. For a long rsync run the
working set is unbounded, so cache pollution is the dominant
signal. The decision rule mirrors
`docs/design/basis-file-io-policy.md` on mmap: turn it on only
when the destination filesystem absorbs unbuffered writes without
amplification. APFS on NVMe is the green case; SMB and NFS mounts
are red because they pay a network round trip per unbuffered
write. The probe lives in section 5.3.

Three known anti-patterns to avoid:

- **Small files re-read soon after write.** A 4 KiB file written
  with `F_NOCACHE` and immediately re-read pays a device read; the
  size threshold in section 5.3 keeps these on the buffered path.
- **Cross-volume copies via SMB/AFP.** Each unbuffered write is a
  separate network operation; latency dominates.
- **`--inplace` over a network filesystem.** The receiver
  overwrites blocks likely cached by another reader; bypassing the
  cache forces re-fetch (section 9.3).

## 3. writev semantics

`writev(2)` accepts an `iovec` array and writes the buffers in
order in a single syscall. On macOS the kernel implementation is
`writev` -> `writev_nocancel` -> `vn_write` in xnu's
`bsd/vfs/vfs_syscalls.c`; for regular files it loops through the
`iovec` and issues `VOP_WRITE` to the underlying filesystem.

What it changes: one syscall, one kernel context switch,
regardless of `iovec` count; atomic with respect to other
`writev`/`write` calls on the same fd from other threads. Short
writes are still possible and the disk-commit thread already
handles them in `write_all_vectored`
(`crates/transfer/src/disk_commit/writer.rs:35-65`).

What it does not change: each `iovec` entry incurs the same
per-byte memory bandwidth as a separate `write`. `writev` saves
syscall overhead, not copy cost. The order on disk is the
concatenation of `iovec` entries; there is no scatter to multiple
offsets (that requires `pwritev`, which macOS implements since
10.10).

The current `ReusableBufWriter::write`
(`crates/transfer/src/disk_commit/writer.rs:90`) already issues a
`writev` of two buffers (the buffered tail plus the new large
chunk) when a chunk crosses the 8 KB direct-write threshold
(`crates/transfer/src/disk_commit/writer.rs:31`). The proof point:
`IoSlice` plumbing exists, `write_vectored` works on a
`std::fs::File`, and the short-write loop is tested in
`crates/transfer/src/disk_commit/tests.rs`. This design
generalises that one site so every flush goes through `writev`.

## 4. Where they compose

Three composition points exist on the receiver hot path.

### 4.1 Basis read with F_NOCACHE

The basis-file reader in
`crates/transfer/src/generator/mod.rs:728` calls
`fast_io::reader_from_path(path, policy)` which today returns
either an `MmapReader` (`crates/fast_io/src/mmap_reader.rs:51`) or
a buffered `File`. The mmap path keeps the basis in the UBC; the
buffered path may or may not, depending on the readahead window.

For files larger than the threshold, the buffered reader sets
`F_NOCACHE` immediately after open. The basis is read once,
sequentially, by the rsum scanner; nothing else on the local
machine reads it during the transfer, so cache retention is pure
waste. The mmap path is unchanged.

### 4.2 writev for the buffered-write fallback

The destination-side `ReusableBufWriter`
(`crates/transfer/src/disk_commit/writer.rs:71`) keeps its 256 KB
reusable buffer (matching upstream's `wf_writeBuf`,
`fileio.c:161`) but routes all flushes through `write_vectored`.
Three call sites change:

- `:97` (combine buffered tail with large incoming chunk) already
  uses `write_all_vectored`. No change.
- `:100` (`self.file.write_all(data)` for a large chunk arriving
  with empty buffer) becomes
  `write_vectored(&[IoSlice::new(data)])`. Functionally identical
  for one buffer, homogeneous with the multi-buffer case.
- `:115` (drain on flush) writes the buffered tail via
  `write_vectored` so the syscall path is uniform.

### 4.3 Combined fast path

State machine for a destination fd opened on a local APFS volume
above the threshold:

```
open(O_WRONLY | O_CREAT | O_TRUNC) -> fd
fcntl(fd, F_NOCACHE, 1)
loop:
    receive chunk_n
    if buf.len() + chunk_n.len() <= buf.capacity():
        buf.extend(chunk_n)
    else:
        writev(fd, [buf, chunk_n]); buf.clear()
on commit:
    if !buf.is_empty(): writev(fd, [buf])
    fsync(fd)
```

The syscall count on a 4 MiB file with 32 KiB chunks drops from
128 (`write` per chunk after buffer fills) to 16 (one `writev`
per 256 KiB cycle). Each `writev` skips the UBC because of
`F_NOCACHE`. Both effects compound.

## 5. API surface change in fast_io

### 5.1 New writer type

A new `WritevWriter` lives in
`crates/fast_io/src/writev_writer.rs`, gated by
`#[cfg(target_os = "macos")]`. It implements the existing
`FileWriter` trait (`crates/fast_io/src/traits.rs:38`) and the
forthcoming `AsyncFileWriter` trait when wired through the
`WritevAsyncWriter` factory branch
(`docs/audits/async-file-writer-trait.md:643`):

```rust
#[cfg(target_os = "macos")]
pub struct WritevWriter {
    file: std::fs::File,
    buf: Vec<u8>,
    no_cache: bool,
}
```

The capability bitset reports `WRITEV | NO_CACHE`
(`docs/audits/async-file-writer-trait.md:295-308`).

### 5.2 Feature gate and disk-commit Writer arm

`fast_io` gains a `writev_macos` feature, default-on for the macOS
target, off otherwise. The disk-commit `Writer` enum
(`crates/transfer/src/disk_commit/writer.rs:141`) gains a third
arm:

```rust
pub(super) enum Writer<'a> {
    Buffered(ReusableBufWriter<'a>),
    #[cfg(all(target_os = "linux", feature = "io_uring"))]
    IoUring { batch: &'a mut fast_io::IoUringDiskBatch },
    #[cfg(all(target_os = "windows", feature = "iocp"))]
    Iocp { batch: &'a mut fast_io::IocpDiskBatch },
    #[cfg(all(target_os = "macos", feature = "writev_macos"))]
    Writev(WritevWriter),
}
```

### 5.3 Runtime opt-in

A `WritevPolicy` enum in `crates/fast_io/src/lib.rs` mirrors
`IoUringPolicy` (`crates/fast_io/src/lib.rs:404`) and `IocpPolicy`
(`:437`):

```rust
#[cfg(target_os = "macos")]
pub enum WritevPolicy { Auto, Enabled, Disabled }
```

`Auto` resolves to `Enabled` when the destination filesystem
reports `apfs` via `statfs(2)` (`f_fstypename`) or is HFS+ on
local storage; the file size hint exceeds 256 KiB (one full
`WRITE_BUF_SIZE`, below which `F_NOCACHE` wastes the device round
trip); and the destination is local (`MNT_LOCAL` from `statfs`).
Otherwise `Auto` resolves to `Disabled` and the receiver falls
through to the existing `Buffered` arm. The probe runs once per
target file, on first write, to amortise `statfs`. A single new
flag, `--writev-macos=auto|on|off`, mirrors
`--io-uring=auto|on|off` and is hidden from default help on
non-macOS targets.

## 6. Interaction with sparse mode

Sparse mode (`-S`, `--sparse`) is the receiver's zero-run
elision: the writer detects 16-byte zero blocks
(`crates/fast_io/src/zero_detect.rs`) and seeks past them rather
than writing zeros. It requires `Seek` because the elision is
`seek(SeekFrom::Current(n))` per zero run.

The current `Writer::buffered_for_sparse`
(`crates/transfer/src/disk_commit/writer.rs:160`) panics if called
on the io_uring or IOCP arm. The same constraint applies to
`Writev`: when sparse mode is active, the receiver must use the
`Buffered` arm because `WritevWriter` does not expose `Seek`.

`writev(2)` cannot preserve zero-runs because it has no
per-`iovec` offset. Skipping a zero run would need either
`pwritev(2)` per non-zero region (defeats the buffered flush) or
`lseek(SEEK_HOLE)`/`lseek(SEEK_DATA)` after the fact (not
portable across HFS+ and APFS, complicates commit). The simpler
answer is to fall through to `Buffered` for sparse mode, the same
way io_uring and IOCP already do. The decision table in
`crates/transfer/src/disk_commit/process.rs:147-170` is extended
to add the `Writev` arm to the "sparse-disabled" column.

`F_NOCACHE` is independent of `writev`: it is a per-fd flag set
once on open. Sparse mode can still benefit because the `seek`
plus `write` pattern still bypasses the UBC for each non-zero
region. The `Buffered` arm therefore gains an optional `F_NOCACHE`
step after open when sparse mode is active and the file is large
enough. Only the `fcntl` is new; the sparse logic in
`crates/transfer/src/disk_commit/process.rs:81-94` is untouched.

A new property test in
`crates/transfer/src/disk_commit/tests.rs` asserts that, for
arbitrary inputs containing zero runs, the bytes written by the
sparse path with `F_NOCACHE` enabled are identical to the bytes
written with it disabled. Gated on `#[cfg(target_os = "macos")]`.

## 7. Comparison with kqueue and dispatch_io

This design is the near-term complement to two longer-term
efforts. The boundaries are firm.

### 7.1 vs. full kqueue (#1385)

The kqueue backend
(`docs/design/macos-kqueue-fast-io.md`) is the strategic
destination: a per-fd readiness model, a single kqueue descriptor
per `KqueueDiskBatch` (mirroring `IocpDiskBatch`), and an
asynchronous submission path for the disk-commit thread.

| Dimension | F_NOCACHE + writev (#1657) | kqueue (#1385) |
|---|---|---|
| Submission | Synchronous, blocks disk thread | Async, `kevent` returns ready fds |
| Latency hiding | None | One file's write overlaps next file's open |
| New module | None | `crates/fast_io/src/kqueue/` |
| `Writer` arm | `Writev` | `Kqueue` |
| Runtime probe | `statfs` + size threshold | `kqueue(2)` availability + sandbox |
| CI scope | None new | New per-fd lifecycle test |

The composition rule, captured in
`docs/design/macos-kqueue-fast-io.md:433-450`: when both designs
land, `KqueueConfig` gains a `f_nocache: bool` field defaulting to
the same `Auto` probe in section 5.3. The `Writev` and `Kqueue`
arms can co-exist; the disk-commit thread picks whichever the
policy probe resolved to.

### 7.2 vs. dispatch_io (#1653, completed)

The `dispatch_io` evaluation closed with "no path forward without
ceding ownership of the disk-commit thread"
(`docs/audits/macos-dispatch-io.md`;
`docs/audits/async-file-writer-trait.md:247-269` reproduces the
call table). The disqualifying constraint was that `dispatch_io`
owns the I/O lifecycle, the buffer copy semantics
(`dispatch_data_t` is reference-counted and may share or copy
depending on alignment), and the dispatch queue topology.
oc-rsync's `BufferPool` and disk-commit thread own those
explicitly. `F_NOCACHE` plus `writev` keeps both boundaries
intact: the buffer is the same `Vec<u8>` the disk thread already
owns, and the syscall path is the same
`std::fs::File::write_vectored` already in use.

### 7.3 Composition matrix

A receiver on macOS sees the following decision order at runtime,
mediated by the `AsyncFileWriter` factory
(`docs/audits/async-file-writer-trait.md:637-650`):

```
factory.create(...)
    if KqueuePolicy::Auto and probe -> Enabled (#1385, future):
        KqueueAsyncWriter (with F_NOCACHE if WritevPolicy Enabled)
    else if WritevPolicy::Auto and probe -> Enabled (#1657):
        WritevAsyncWriter (with F_NOCACHE)
    else:
        StdAsyncWriter (crates/fast_io/src/traits.rs:120)
```

Today every macOS receiver uses `StdAsyncWriter`. This design
closes the second branch without committing to the larger kqueue
work.

## 8. Test plan

### 8.1 macOS CI matrix coverage

The existing macOS CI job
(`.github/workflows/ci.yml:364-401`) runs
`cargo nextest run --locked -p core -p engine -p cli --all-features`
on `macos-latest` for stable, beta, and nightly toolchains. This
design adds, all gated on `#[cfg(target_os = "macos")]`:

- Unit tests in `crates/fast_io/src/writev_writer.rs` for the
  short-write loop.
- An integration test
  `crates/fast_io/tests/writev_macos_smoke.rs` that opens a file,
  sets `F_NOCACHE`, writes via `writev`, and verifies the
  content matches the input.
- A property test in
  `crates/transfer/src/disk_commit/tests.rs` for the
  sparse-with-F_NOCACHE case (section 6).

No new CI workflow file is needed.

### 8.2 Filesystem matrix on developer hardware

The probe in section 5.3 must succeed on APFS and fall through on
SMB and NFS. Local APFS is exercised by CI. SMB and NFS are
exercised by a developer-only script
`tools/macos/test_writev_filesystems.sh`, not run in CI (no
infrastructure for SMB/NFS in `macos-latest`).

### 8.3 Throughput regression test

A `cargo bench` micro-benchmark in
`crates/fast_io/benches/writev_macos.rs` measures per-syscall
throughput of `Buffered`, `Buffered+F_NOCACHE`, and
`Writev+F_NOCACHE`. It writes a synthetic in-memory source to a
temp file on the runner's APFS volume and asserts the ordering
`Writev+F_NOCACHE > Buffered+F_NOCACHE > Buffered` for files
above 1 MiB. Opt-in via a `MACOS_BENCH=1` environment variable so
CI noise does not flake on the small machine class.

### 8.4 Interop test

The existing macOS interop driver (`tools/ci/run_interop.sh`)
exercises the receiver against upstream rsync 3.0.9, 3.1.3, and
3.4.1. `F_NOCACHE` and `writev` are receiver-internal, so the
interop matrix needs no new entry. The runner only confirms
bit-identical output across `--writev-macos=on` and `=off` for a
representative sample. Two harness rows (`apfs-writev-on` and
`apfs-writev-off`) are added.

## 9. Open questions

### 9.1 F_NOCACHE on rename targets

`commit_file` (`crates/transfer/src/disk_commit/process.rs:99`)
renames the temp file into place. If the destination already
exists with cached pages, the rename invalidates them. Open
question: does the caller see stale data if a concurrent reader
holds the old fd? xnu's `vfs_rename` answers no for the rename
itself, but the old fd may still see the old content until close.
We assume this is acceptable (the same behaviour as
`BufWriter<File>` plus rename); the property test in section 6
should include a rename-during-write fixture.

### 9.2 Threshold tuning

Section 5.3 proposes 256 KiB as the file-size threshold. That is
an educated guess. The benchmark in section 8.3 should be
parameterised by threshold to find the crossover on each tested
CPU class. The final number lands as a constant in
`crates/fast_io/src/writev_writer.rs`, not as a CLI flag.

### 9.3 Coexistence with `--inplace`

`--inplace` writes back to the existing destination. With
`F_NOCACHE` set, concurrent readers pay a device round trip per
access. Conservative answer: disable `Writev` when `--inplace` is
on, the same way io_uring is gated by
`disk_commit::config::DiskCommitConfig`
(`crates/transfer/src/disk_commit/config.rs:46`). Open question:
gate `F_NOCACHE` only and keep `writev`? Probably yes; `writev`
itself is harmless on `--inplace`.

### 9.4 SMB attached over USB-C tethering

Mac mini M2 over USB-C tethered Ethernet to an SMB server is a
common developer setup. The `MNT_LOCAL` check catches it.
`F_NOCACHE` is advisory on remote filesystems; if a future macOS
release makes it mandatory on SMB, the probe needs an explicit
`darwin_version` gate.

### 9.5 fcopyfile interaction

`fcopyfile_impl`
(`crates/fast_io/src/platform_copy/dispatch.rs:186-209`) is the
local-copy executor's macOS fast path. It does not interact with
`WritevWriter` because local copy bypasses the disk-commit thread
entirely (`crates/fast_io/src/platform_copy/mod.rs:13`). Open
question: should the local-copy executor also set `F_NOCACHE` on
the destination fd before `fcopyfile`? The call accepts an open
fd, so the change is one `fcntl`. Tracked as a follow-up.

### 9.6 AppleDouble side-files (#1907, in progress)

The AppleDouble support in #1907 writes `._` side-files holding
the resource fork and Finder metadata. Those side-files are tiny
(under 4 KiB), so the section 5.3 size threshold keeps them on
the `Buffered` arm. Open question: confirm with #1907 that the
side-file commit path does not pre-stat the target expecting it
in the UBC. Tracked in #1907; this design does not block on it.

## 10. References

- `crates/fast_io/src/platform_copy/dispatch.rs:62-95,184-209` -
  macOS copy dispatch and `fcopyfile` FFI wrapper.
- `crates/fast_io/src/traits.rs:38,120` - `FileWriter` trait and
  `StdFileWriter` universal fallback.
- `crates/fast_io/src/lib.rs:404,437` - `IoUringPolicy` and
  `IocpPolicy` templates.
- `crates/fast_io/src/zero_detect.rs`,
  `crates/fast_io/src/mmap_reader.rs:51` - SIMD zero-run
  detection and basis-file reader.
- `crates/transfer/src/disk_commit/writer.rs:20,31,35-65,71,141,160` -
  `WRITE_BUF_SIZE`, `DIRECT_WRITE_THRESHOLD`, `write_all_vectored`,
  `ReusableBufWriter`, `Writer` enum, `buffered_for_sparse`.
- `crates/transfer/src/disk_commit/process.rs:81-94,99` - sparse
  write loop and `commit_file` rename site.
- `crates/transfer/src/disk_commit/config.rs:46` - `use_sparse`
  flag template.
- `.github/workflows/ci.yml:364-401` - macOS CI job.
- `docs/design/macos-kqueue-fast-io.md:433-450` - longer-term
  direction (#1385) and explicit composition rule.
- `docs/audits/async-file-writer-trait.md:247-269,637-650` -
  unified writer trait (#1655), `dispatch_io` summary, macOS
  factory branches.
- `docs/audits/macos-dispatch-io.md` (#1653, completed),
  `docs/audits/cross-platform-parity-matrix.md`,
  `docs/audits/apple-fs-roundtrip.md` (#1907).
- `man 2 fcntl`, `man 2 writev`, `man 2 statfs`.
