# Windows CopyFileEx Fast Path - Implementation Plan (#1414)

Status: Design. Owner: fast_io. Tracking issue: #1414.

## Cross-References

- **#1272 (done):** CopyFileEx optimization path stub. Established the dispatch
  point in `fast_io::copy::dispatch()` and a Windows-only stub returning
  `Ok(None)` so callers fall through to the user-space copier. No syscall
  wired yet.
- **#1749 (pending):** Parity with the Linux `copy_file_range` fast path.
  Treats `CopyFileExW` on Windows as the platform analog: same orchestration
  layer, same fallback semantics, same metrics. Closing #1414 is a
  prerequisite for #1749.

## Implementation Plan

1. Wire `CopyFileExW` from the `windows` crate
   (`Win32::Storage::FileSystem::CopyFileExW`) behind
   `#[cfg(windows)]` inside `fast_io`. Expose a safe `copy_file_ex(src, dst,
   progress, cancel)` that returns `Ok(bytes_copied)` or a typed error.
2. Flags: `COPY_FILE_NO_BUFFERING | COPY_FILE_RESTARTABLE`. The first bypasses
   the cache for large transfers; the second enables resume after interrupt
   - both match the contract documented for `copy_file_range` parity.
3. Progress: pass a `LPPROGRESS_ROUTINE` trampoline that forwards
   `(total_bytes, transferred)` into the existing `--progress` reporter
   through an `Arc<ProgressSink>`. Routine returns `PROGRESS_CONTINUE` or
   `PROGRESS_CANCEL` driven by the shared cancel token.
4. Lifecycle: open paths via `HSTRING`/`PCWSTR` from `windows::core` to avoid
   manual UTF-16 marshalling. The unsafe FFI lives in `fast_io` only,
   surfaced as a safe API per the Unsafe Code Policy.
5. Tests: golden + integration in `crates/fast_io/tests/windows_copyfileex.rs`
   gated on `#[cfg(windows)]`, plus a cross-platform stub returning
   `Unsupported` so `core` compiles unchanged elsewhere.

## Trigger Conditions

Dispatch only when **all** hold:

- Target is `windows`.
- `src` and `dst` resolve to the **same NTFS volume** (compare
  `GetVolumePathNameW` outputs).
- Volume reports `FILE_SUPPORTS_BLOCK_REFCOUNTING` or
  `FILE_SUPPORTS_OBJECT_IDS` via `GetVolumeInformationW`, indicating
  server-side / refcount copy semantics are honoured.
- Transfer is a whole-file copy (no `--inplace`, no `--append`, no delta
  reconstruction). Any of those falls back to the user-space pipeline.

Otherwise return `Ok(None)` and let `fast_io::copy::dispatch()` proceed to
the standard read/write path - identical to the Linux fallback contract.

## Risks and Mitigations

- **`COPY_FILE_NO_BUFFERING` alignment:** requires sector-aligned source
  offsets, destination offsets, and buffer sizes. We always start at offset
  zero and copy the full file, so the alignment is satisfied for the
  whole-file trigger above. If a future caller ever requests a partial copy,
  drop the flag for that call and document it next to the dispatch site.
- **Cross-volume copies:** `CopyFileExW` succeeds across volumes but loses
  the server-side acceleration; we explicitly gate on same-volume to avoid
  pretending we have a fast path when we don't. Cross-volume requests fall
  through to the user-space copier.
- **Progress callback re-entrancy:** the routine fires on a worker thread
  owned by Windows. The `ProgressSink` is `Send + Sync`; no locks held
  across the boundary.
- **Restartable state files:** `COPY_FILE_RESTARTABLE` writes a small
  sidecar. We clean it up on success and on terminal cancel, matching how
  the temp-file commit path tidies up partials.
