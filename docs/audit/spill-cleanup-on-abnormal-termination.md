# SRO-11: Spill Temp-File Cleanup on Abnormal Termination

Audit of the spill layer's temp-file lifecycle under normal exit, panic,
SIGKILL, and OOM-kill scenarios.

## Scope

Files examined:

- `crates/engine/src/concurrent_delta/spill/tempfile.rs` - backend abstraction
- `crates/engine/src/concurrent_delta/spill/buffer/mod.rs` - `SpillableReorderBuffer` struct
- `crates/engine/src/concurrent_delta/spill/buffer/lifecycle.rs` - constructors, accessors
- `crates/engine/src/concurrent_delta/spill/buffer/spill.rs` - spill-to-disk path
- `crates/engine/src/concurrent_delta/spill/buffer/reload.rs` - reload-from-disk path
- `crates/engine/src/concurrent_delta/spill/buffer/insert.rs` - insert paths
- `crates/engine/src/concurrent_delta/consumer/spawn.rs` - thread spawn, backend construction
- `crates/engine/src/concurrent_delta/consumer/loops.rs` - reorder loop lifecycle

## Background

The spill layer gives the `SpillableReorderBuffer` bounded memory by
serializing excess reorder-buffer entries to a temporary file when the
in-memory byte budget is exceeded. Two backend flavours exist:

| Backend | When selected | Construction |
|---------|---------------|--------------|
| `SpillBackend::Spooled` | `spill_dir` is `None` (default) | `tempfile::SpooledTempFile::new(1_048_576)` |
| `SpillBackend::Directory` | `spill_dir` is `Some(path)` | `tempfile::tempfile_in(dir)` |

Both use the `tempfile` crate (v3.27.0).

## Finding 1: Temp Files Are Anonymous

### Spooled backend (default)

`tempfile::SpooledTempFile` holds data in an in-memory `Vec<u8>` until
it exceeds the 1 MB cursor threshold, at which point it rolls over to an
anonymous OS tempfile created via `tempfile::tempfile()`. The underlying
`tempfile::tempfile()` function:

- **Linux**: attempts `O_TMPFILE` first (truly anonymous - no directory
  entry is ever created). Falls back to creating a named file, opening it,
  then immediately unlinking it. Either way the file has no directory entry
  once the `File` handle exists.
- **macOS/BSD**: creates a named file, opens it, then unlinks it.
- **Windows**: uses `FILE_FLAG_DELETE_ON_CLOSE` via `CreateFile`, so the
  OS deletes the file when the last handle closes.

In all cases the on-disk file is anonymous (unlinked/flagged for deletion)
once the `File` is returned. Closing the file descriptor - whether via
explicit drop, process exit, or kernel reaping - reclaims the disk space.

### Directory backend

`tempfile::tempfile_in(dir)` is identical to `tempfile::tempfile()` except
it places the anonymous file inside the caller-supplied directory. The
same unlink-after-open (or `O_TMPFILE`) logic applies. The resulting
`std::fs::File` has no directory entry.

**Verdict: both backends produce anonymous temp files.** No named files
persist after the file descriptor is closed.

## Finding 2: No Custom Drop Impl on SpillableReorderBuffer

`SpillableReorderBuffer<T>` does not implement `Drop`. When the struct is
dropped, Rust's implicit drop runs field destructors in declaration order:

1. `inner: ReorderBuffer<T>` - in-memory ring, no I/O.
2. `spill_file: Option<SpillBackend>` - this is the critical field.

`SpillBackend` is an enum holding either a `SpooledTempFile` or a
`std::fs::File`. Both implement `Drop`:

- `SpooledTempFile::drop()` drops its inner `File` (if it rolled over to
  disk), which closes the fd. Since the file is anonymous, the OS reclaims
  the space.
- `File::drop()` closes the fd. Same result.

**Verdict: implicit Drop is sufficient.** The anonymous-file strategy
means closing the fd is all that is needed for cleanup.

## Finding 3: Panic in a Worker Thread

The `SpillableReorderBuffer` lives on the `delta-reorder` thread
(see `spawn.rs`). If that thread panics:

1. Rust's panic machinery unwinds the stack (unless `panic = "abort"`).
2. Stack unwinding runs destructors for all local variables.
3. The `SpillableReorderBuffer` (owned as a local in `run_spillable_loop`)
   is dropped, closing the anonymous temp file fd.
4. The `delta-reorder` thread's `JoinHandle` propagates the panic to the
   parent when joined.

If the panic happens inside a rayon worker (the `delta-drain` thread
runs `drain_parallel_into` inside a `rayon::scope`), the rayon scope
catches the panic and propagates it out of the scope. The
`SpillableReorderBuffer` is not owned by rayon workers - it lives on the
reorder thread - so rayon panics do not affect its drop path.

**Verdict: panic cleanup is safe.** Stack unwinding drops the buffer,
which closes the anonymous fd, which reclaims disk space.

Edge case: if the process is compiled with `panic = "abort"`, a panic
immediately terminates the process. This is equivalent to SIGKILL from
the perspective of fd cleanup - see Finding 4.

## Finding 4: SIGKILL / OOM-Kill

SIGKILL and OOM-kill terminate the process immediately. No destructors
run. No signal handlers execute. The kernel reaps all resources:

1. All open file descriptors are closed by the kernel.
2. Anonymous files (unlinked or `O_TMPFILE`) have their reference count
   decremented. Since no directory entry holds another reference, the
   count reaches zero and the inode is freed - disk space is reclaimed.
3. Memory-mapped regions (if any) are unmapped.

This is the key advantage of anonymous temp files over named temp files:
the kernel guarantees cleanup on process death because the only reference
is the process's own fd table.

**Verdict: SIGKILL and OOM-kill are safe.** The kernel reclaims all
anonymous temp file space when the process dies, regardless of how it dies.

## Finding 5: The Spill Directory Is Not Cleaned Up

When using `with_spill_dir(dir)`, the constructor calls
`fs::create_dir_all(&dir)` to ensure the directory exists. However:

- No code removes the directory on normal exit.
- No code removes the directory on drop.
- The directory is caller-supplied (e.g., `/tmp/oc-rsync-spill`), not
  system-managed.

This is a minor cosmetic issue, not a data-safety concern:

- The anonymous temp files inside the directory are cleaned up by fd close.
- The directory itself is empty after cleanup completes.
- If the directory is under `/tmp`, the OS or distro tmpfs reaper
  eventually removes it.
- If the operator supplied a custom directory, they presumably manage its
  lifecycle.

**Verdict: empty directory may persist, but no data leak.** This matches
the design intent documented in `lifecycle.rs`:

> "caller is responsible for ensuring the directory exists"

The caller creates it; the caller (or the OS) removes it.

## Finding 6: SpooledTempFile Internal Cleanup

`tempfile::SpooledTempFile` wraps an enum:

```
enum SpooledInner {
    InMemory(Cursor<Vec<u8>>),
    OnDisk(File),
}
```

When the spool has not rolled over to disk (data stayed under 1 MB), only
an in-memory `Vec<u8>` exists - no file descriptor, no disk allocation.
Drop frees the heap buffer.

When it has rolled over, the `OnDisk(File)` variant holds an anonymous
`File` whose drop closes the fd and reclaims space, as described above.

**Verdict: SpooledTempFile handles both paths correctly.**

## Finding 7: File Descriptor Leak Scenarios

The only fd opened by the spill layer is the one inside `SpillBackend`.
It is opened lazily on first spill (`write_record` calls `open_backend`
only when `spill_file` is `None`). Potential leak vectors:

1. **Double-open**: `recreate_spill_dir` sets `self.spill_file = None`
   before re-creating the directory. This drops the old `SpillBackend`,
   closing the stale fd, before the next `write_record` opens a new one.
   No leak.

2. **Early return**: if an error occurs after opening the spill file but
   before the `SpillableReorderBuffer` is dropped, the file is still
   owned by the struct. When the struct is dropped (via unwinding or
   normal scope exit), the fd closes. No leak.

3. **Forgotten buffer**: if a caller wraps the buffer in `ManuallyDrop`
   or `mem::forget`s it, the fd leaks until process exit. This is a
   general Rust footgun, not specific to the spill layer. No production
   code path does this.

**Verdict: no fd leak under normal or abnormal termination.**

## Summary Table

| Scenario | Destructors run? | Temp file cleaned up? | Directory cleaned up? |
|----------|------------------|-----------------------|-----------------------|
| Normal exit | Yes | Yes (fd close) | No (empty dir persists) |
| Panic (unwind) | Yes | Yes (fd close) | No |
| Panic (abort) | No | Yes (kernel reaps anonymous fd) | No |
| SIGKILL | No | Yes (kernel reaps anonymous fd) | No |
| OOM-kill | No | Yes (kernel reaps anonymous fd) | No |
| Thread panic (rayon) | Yes (reorder thread unwinds) | Yes (fd close) | No |

## Conclusion

The spill layer's use of anonymous temp files (via `tempfile::tempfile()`
and `tempfile::tempfile_in()`) provides robust cleanup under all
termination scenarios. The `tempfile` crate's strategy of unlinking the
file immediately after creation means the kernel is the sole cleanup
authority - no application-level `Drop` logic is needed for data safety.

The only residual artifact is the empty spill directory when
`with_spill_dir` is used. This is a cosmetic concern with no
data-safety implications, and matches the documented contract that the
caller manages the directory lifecycle.

**No code changes are needed.** The current design is correct.
