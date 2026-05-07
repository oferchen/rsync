# Sparse writer Decorator pattern

Tracker: #2132. Branch: `docs/sparse-writer-decorator-2132`. No code changes.

## Scope

Audit of `--sparse` write paths to determine whether the existing zero-run
detection and hole-creation logic already conforms to the Decorator pattern,
and what shape a true `SparseWriter<W: Write>` decorator would take.

The relevant modules are:

- `crates/transfer/src/delta_apply/sparse.rs` - `SparseWriteState` used by the
  receiver disk-commit thread.
- `crates/transfer/src/disk_commit/process.rs` - sparse dispatch in the
  per-file commit loop.
- `crates/transfer/src/disk_commit/writer.rs` - `Writer` enum that gates
  sparse mode against the buffered backend.
- `crates/transfer/src/constants.rs` - `leading_zero_count` /
  `trailing_zero_count` (16-byte `u128` SWAR).
- `crates/engine/src/local_copy/executor/file/sparse/` - parallel local-copy
  pipeline with a `SparseWriter` wrapper, `SparseWriteState`, and
  `punch_hole` helper.
- `crates/fast_io/src/zero_detect.rs` - SIMD `find_first_nonzero` /
  `is_all_zeros` primitives (AVX2/SSE2/NEON + 16-byte `u128` scalar).

## 1. Existing code shape

There are two parallel sparse pipelines today, one in `transfer` (delta apply
via the disk-commit thread) and one in `engine` (local-copy executor). Both
share the same conceptual state machine but neither is a Decorator.

### 1.1 transfer disk-commit pipeline

`SparseWriteState` is a stateful helper, not a writer.

`crates/transfer/src/delta_apply/sparse.rs:17`:

```rust
pub struct SparseWriteState {
    pending_zeros: u64,
}
```

It exposes `write<W: Write + Seek>(&mut self, writer: &mut W, data: &[u8])`
(`sparse.rs:63`), which:

1. Slices the input into `CHUNK_SIZE` (32 KiB) segments.
2. Calls `leading_zero_count` and `trailing_zero_count`
   (`crates/transfer/src/constants.rs:97`, `:125`) to find the leading and
   trailing zero runs - both walk the buffer in 16-byte `u128` strides.
3. Accumulates leading zeros into `pending_zeros`, flushes them by
   `writer.seek(SeekFrom::Current(n))` when non-zero data follows
   (`sparse.rs:42`), then writes only the non-zero middle slice via
   `writer.write_all`.
4. `finish` (`sparse.rs:100`) seeks `pending - 1` bytes forward and writes a
   single trailing `0u8` so the file's logical size is correct - this is the
   well-known upstream "tail byte" trick from `fileio.c:write_sparse`.

The dispatch site is `crates/transfer/src/disk_commit/process.rs`, which
keeps the writer and the sparse state as two separate locals:

```rust
// process.rs:43
let mut output = make_writer(file, write_buf, ..., config.use_sparse, ...);
let mut sparse_state = if config.use_sparse {
    Some(SparseWriteState::default())
} else { None };
...
// process.rs:81
if let Some(ref mut sparse) = sparse_state {
    sparse.write(output.buffered_for_sparse(), &data)?;
} else {
    output.write_chunk(&data)?;
}
```

`buffered_for_sparse` (`crates/transfer/src/disk_commit/writer.rs:160`) is a
type-level escape hatch: the io_uring (`Writer::IoUring`) and IOCP
(`Writer::Iocp`) variants intentionally lack `Seek`, so the sparse path must
narrow to `ReusableBufWriter`. `make_writer`
(`crates/transfer/src/disk_commit/process.rs:269`) refuses the batched
backends whenever `use_sparse` is set.

`process_whole_file` (`process.rs:149`) duplicates the same `if use_sparse {
state.write; state.finish } else { write_chunk }` branch for the
single-message fast path.

### 1.2 engine local-copy pipeline

`crates/engine/src/local_copy/executor/file/sparse/writer.rs` already
exposes a type called `SparseWriter`:

```rust
pub struct SparseWriter {
    file: fs::File,
    sparse_enabled: bool,
    state: SparseWriteState,
}
```

It is *not* a Decorator. The struct owns a concrete `fs::File`, takes
absolute offsets via `write_region(offset, data)` (`writer.rs:72`), and the
`sparse_enabled: bool` toggles between `write_sparse_chunk` and a plain
`seek + write_all`. `finish` calls `set_len` + `sync_all` directly on the
owned `fs::File` and consumes `self`.

The accompanying state lives in
`crates/engine/src/local_copy/executor/file/sparse/state.rs:23`
(`SparseWriteState { pending_zero_run, zero_run_start_pos }`) and offers two
flush strategies - `flush` (seek-only) and `flush_with_punch_hole`
(`fallocate(FALLOC_FL_PUNCH_HOLE | FALLOC_FL_KEEP_SIZE)` from
`hole_punch.rs:44`, falling back to `ZERO_RANGE` then a buffered
zero-write). Strategy selection is hard-coded by the caller picking which
flush method to call, not by composition.

### 1.3 Underlying primitives

Two zero-detect implementations coexist:

- `crates/transfer/src/constants.rs:97` - portable 16-byte `u128`
  `leading_zero_count` / `trailing_zero_count`. The compiler auto-vectorises
  these on x86-64 and aarch64.
- `crates/fast_io/src/zero_detect.rs` - explicit AVX2 (32 B/iter), SSE2
  (16 B/iter), NEON (16 B/iter), and a `u128` scalar fallback dispatched via
  `OnceLock`. `find_first_nonzero` / `is_all_zeros` are public.

The `fast_io` SIMD primitives are *not* used by either sparse pipeline
today. Both call into `transfer/src/constants.rs` instead.

### 1.4 Verdict

Neither pipeline matches the Decorator pattern:

- `transfer::SparseWriteState` is a state object the caller must thread
  through every write call, not a `Write` impl. The caller still owns and
  drives the inner writer.
- `engine::SparseWriter` is a concrete wrapper hard-coded to `fs::File`
  with a positional `write_region` API rather than `Write::write`. It is a
  Facade over `(File, SparseWriteState)`, not a Decorator over arbitrary
  `W: Write`.
- The toggle is a runtime `bool` (`sparse_enabled`, `use_sparse`) checked
  on every chunk, not a compile-time decoration.

## 2. Proposed Decorator

Refactor both pipelines onto a single `SparseWriter<W>` decorator that
implements `Write + Seek` over any `W: Write + Seek`, intercepts zero runs,
and flushes them as `seek(Current(n))` calls on the inner writer.

### 2.1 Trait shape

```rust
// crates/transfer/src/delta_apply/sparse.rs (new)

/// Decorator that converts zero runs in the input stream into seek-only
/// commits on the wrapped writer, producing filesystem holes.
///
/// Mirrors upstream rsync's `write_sparse()` (fileio.c) and is wire-equal
/// to the existing `SparseWriteState` flow.
pub struct SparseWriter<W: Write + Seek> {
    inner: W,
    state: SparseWriteState,
}

impl<W: Write + Seek> SparseWriter<W> {
    pub fn new(inner: W) -> Self { ... }

    /// Flushes pending zeros, writes the upstream "tail byte" if any, and
    /// returns the wrapped writer so callers can fsync, set_len, or
    /// rename the underlying file.
    pub fn finish(mut self) -> io::Result<W> { ... }

    pub fn get_ref(&self) -> &W { &self.inner }
    pub fn get_mut(&mut self) -> &mut W { &mut self.inner }
}

impl<W: Write + Seek> Write for SparseWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        // existing 32 KiB-segment scan + flush + write logic.
        self.state.write(&mut self.inner, buf)
    }

    fn flush(&mut self) -> io::Result<()> { self.inner.flush() }
}

impl<W: Write + Seek> Seek for SparseWriter<W> {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        self.state.flush(&mut self.inner)?;
        self.inner.seek(pos)
    }
}
```

The `Seek` impl is required because pending zeros must be drained before
any explicit caller-driven seek; otherwise the sparse hole drifts.

### 2.2 Hole-punch variant via composition

The engine's `flush_with_punch_hole` strategy maps cleanly onto a second
decorator that owns the same state machine but flushes via `fallocate`
instead of `seek`. Either approach works:

- **Strategy injection**: parameterise `SparseWriter` with a
  `Flusher` trait (`Flusher::flush(&mut W, pending: u64) -> io::Result<()>`)
  and ship `SeekFlusher` plus `PunchHoleFlusher` (Linux-only) impls. Pure
  composition, no enum.
- **Two sibling decorators**: `SparseSeekWriter<W>` and
  `SparsePunchHoleWriter<W: AsRawFd>`. Simpler types, slightly more
  duplication.

Strategy injection is preferred because the rest of the code base already
uses the Strategy pattern for codec / checksum / detect dispatch
(`SparseDetectStrategy` lives next door in
`crates/engine/src/local_copy/executor/file/sparse/mod.rs:46`).

### 2.3 Trait bounds and platform constraints

- `W: Write + Seek` is mandatory. Sparse holes are produced by
  `seek(Current(n))` past unwritten regions; without `Seek` the decorator
  cannot produce holes. This is also the bound enforced today via
  `Writer::buffered_for_sparse` (`disk_commit/writer.rs:160`).
- `W: AsRawFd` (Unix) or `W: AsRawHandle` (Windows) is required only for
  the punch-hole flusher; the seek flusher works on any `Write + Seek`,
  including `Cursor<Vec<u8>>` (the existing tests in
  `delta_apply/sparse.rs:118` rely on this).
- The decorator is `Send` whenever `W: Send`. The disk-commit thread holds
  the writer alone, so this matches today's invariants.

### 2.4 Migration of existing call sites

| Site | Current shape | Post-refactor |
|---|---|---|
| `crates/transfer/src/disk_commit/process.rs:43-94` | `Writer + Option<SparseWriteState>`, dispatched by `if let Some(sparse)` on every chunk | `match config.use_sparse { true => SparseWriter::new(buffered), false => buffered }` once at file open; the per-chunk loop reduces to `output.write_all(&data)` |
| `crates/transfer/src/disk_commit/process.rs:175` (`process_whole_file`) | Same dual branch | Same: build the decorator at top, single `write_all` |
| `crates/transfer/src/disk_commit/writer.rs:160` (`buffered_for_sparse`) | Type-level downcast that panics on batched variants | Removed. The decorator only wraps the `Buffered` variant; the enum is split before sparse decoration |
| `crates/engine/src/local_copy/executor/file/sparse/writer.rs:34` | `SparseWriter { file, sparse_enabled: bool, state }` | `SparseWriter<fs::File>` always-on, with the `sparse_enabled = false` path served by the inner `fs::File` directly (no decorator) |
| `crates/engine/.../sparse/state.rs:135` (`write_sparse_chunk`) | Free function called by `SparseWriter::write_region` | Becomes `<SparseWriter as Write>::write`; `write_region` delegates `seek(Start(offset))` then `write_all` |

The `write_region(offset, data)` signature can stay as a thin convenience on
top of `Write + Seek`:

```rust
impl<W: Write + Seek> SparseWriter<W> {
    pub fn write_region(&mut self, offset: u64, data: &[u8]) -> io::Result<()> {
        self.seek(SeekFrom::Start(offset))?;
        self.write_all(data)
    }
}
```

### 2.5 Single-seek-per-zero-run invariant

The existing implementation already preserves the
"single seek per zero run" invariant (CLAUDE.md performance section). The
decorator must preserve it as well. The two preconditions are:

1. `accumulate(n)` must never seek - it only updates `pending_zeros`. The
   current code does this correctly
   (`delta_apply/sparse.rs:30`, `engine/.../state.rs:31`) and the proposed
   decorator delegates to `SparseWriteState::accumulate`.
2. `flush` issues exactly one logical seek per drained run, chunked into
   `i64::MAX` steps only when `pending_zeros > i64::MAX`
   (`delta_apply/sparse.rs:46-52`). For real workloads this loop runs once.
   The decorator's `Write::write` calls `flush` only when non-zero data
   follows, which is the same trigger as today.

A property test should be added asserting:

- `seek_count(SparseWriter::new(cursor).write_all(&[0; N]).finish())` is
  exactly `0` (everything stays pending until `finish` writes the tail
  byte).
- `seek_count` for a buffer with `k` discrete zero runs is exactly `k`,
  regardless of run length.

`crossbeam`-style instrumentation is unnecessary; a `CountingSeek` test
double around `Cursor<Vec<u8>>` suffices. The existing tests in
`delta_apply/sparse.rs:118` and `engine/.../sparse/tests.rs` already use
`Cursor`-backed fixtures.

### 2.6 SIMD primitive consolidation

While refactoring, replace the duplicated 16-byte `u128` scanners in
`crates/transfer/src/constants.rs:97`/`:125` with calls into
`fast_io::zero_detect::find_first_nonzero`. The `transfer` helpers can be
re-expressed as:

```rust
pub fn leading_zero_count(bytes: &[u8]) -> usize {
    fast_io::zero_detect::find_first_nonzero(bytes)
}

pub fn trailing_zero_count(bytes: &[u8]) -> usize {
    let mirror = bytes.iter().rev();
    // Or: extend fast_io with `find_last_nonzero` and call that.
    ...
}
```

`fast_io::zero_detect` already provides AVX2 / SSE2 / NEON dispatch with
the same `u128` scalar fallback the transfer crate hand-rolled. This
removes ~80 lines of duplicated SWAR code (`constants.rs:96-149`) and
gives the sparse decorator immediate SIMD acceleration on every supported
platform. A `find_last_nonzero` companion is the only addition needed in
`fast_io` for full coverage.

## 3. Implementation sites

For the eventual implementation tracker:

- New file: `crates/transfer/src/delta_apply/sparse_writer.rs` (decorator).
- Edit `crates/transfer/src/delta_apply/mod.rs` to re-export
  `SparseWriter`; keep `SparseWriteState` as `pub(crate)` for the
  decorator's private use (or fold into the new module).
- Replace dual branches in
  `crates/transfer/src/disk_commit/process.rs:43-94` and `:149-206`.
- Remove `Writer::buffered_for_sparse`
  (`crates/transfer/src/disk_commit/writer.rs:160-174`); refactor
  `make_writer` (`process.rs:269`) so the sparse selection happens by
  wrapping the `Buffered` variant in a `SparseWriter`, never returning a
  batched variant under sparse mode.
- Refactor `crates/engine/src/local_copy/executor/file/sparse/writer.rs`
  to use the shared decorator from `transfer` (or, if the engine should
  not depend on `transfer`, hoist the decorator into a third location -
  `fast_io` is the natural home given the existing
  `zero_detect` module).
- Add `find_last_nonzero` to `crates/fast_io/src/zero_detect.rs` and
  delete `crates/transfer/src/constants.rs:96-149` in favour of the
  consolidated primitives.

## 4. Performance notes

- The decorator preserves the current zero-allocation hot path. The state
  is a single `u64`, the inner writer is borrowed by `&mut`, and there is
  no boxing.
- Wrapping is free at runtime: `SparseWriter<ReusableBufWriter>` is a
  monomorphised type, the `Write` trait calls inline through, and the
  compiler can fuse `SparseWriter::write` with `ReusableBufWriter::write`
  the same way it does today with the manual `if let Some(sparse)`
  branch.
- Hot-path SIMD acceleration becomes available for free once the
  primitives consolidate onto `fast_io::zero_detect` (32 B/iter on AVX2
  vs the current 16 B/iter `u128`).
- The single-seek-per-zero-run invariant is preserved by `accumulate` +
  lazy `flush`; tests must enforce it.
- `seek` on `ReusableBufWriter` already flushes the buffer
  (`disk_commit/writer.rs:124`); the decorator's `Seek` impl therefore
  costs at most one extra `flush` call per caller-driven seek, which is
  free in the common case (sparse seeks are *always* preceded by buffered
  writes that the inner writer would have flushed anyway).
