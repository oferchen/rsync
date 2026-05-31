# IOCP Error Conditions Audit (WSD-4)

Audit of error handling in the Windows I/O Completion Ports path
(`crates/fast_io/src/iocp/`). Identifies all error paths, classifies
them by severity, inventories test coverage, and compares with the
io_uring equivalent.

---

## 1. Error Path Inventory

### 1.1 completion_port.rs

| Operation | Windows API | Possible Errors | Current Handling |
|-----------|-------------|-----------------|------------------|
| `CompletionPort::new` | `CreateIoCompletionPort(INVALID_HANDLE_VALUE, null, 0, max_threads)` | Out of memory, invalid parameter | Returns `io::Error::last_os_error()` when handle is null |
| `CompletionPort::associate` | `CreateIoCompletionPort(file_handle, port_handle, key, 0)` | `ERROR_INVALID_PARAMETER` (handle not overlapped), `ERROR_INVALID_HANDLE` | Returns `io::Error::last_os_error()` when result is null |
| `CompletionPort::drop` | `CloseHandle(handle)` | `ERROR_INVALID_HANDLE` (double-close) | Return value ignored (silent) |

### 1.2 file_writer.rs

| Operation | Windows API | Possible Errors | Current Handling |
|-----------|-------------|-----------------|------------------|
| `IocpWriter::open_with_disposition` | `CreateFileW` | `ERROR_FILE_NOT_FOUND`, `ERROR_PATH_NOT_FOUND`, `ERROR_ACCESS_DENIED`, `ERROR_SHARING_VIOLATION` | Returns `io::Error::last_os_error()` when handle is `INVALID_HANDLE_VALUE` |
| Port association | `CompletionPort::associate` | `ERROR_INVALID_PARAMETER` | Propagated via `?` |
| `SetFileCompletionNotificationModes` | `SetFileCompletionNotificationModes` | Failure on pre-Vista (unsupported) | Return value ignored (non-fatal, documented) |
| `write_at` | `WriteFile` | `ERROR_IO_PENDING` (expected), `ERROR_INVALID_PARAMETER` (code 87), `ERROR_DISK_FULL`, `ERROR_NOT_ENOUGH_MEMORY` | `ERROR_IO_PENDING` enters completion wait; code 87 upgraded to `IocpError::InvalidOperation`; others returned as-is |
| `write_at` completion | `GetQueuedCompletionStatus` | `WAIT_TIMEOUT`, `ERROR_ABANDONED_WAIT_0`, `ERROR_OPERATION_ABORTED` | Returns classified error via `classify_overlapped_error` |
| `flush_buffer` | (internal) | Zero-byte write | Returns `io::ErrorKind::WriteZero` |
| `create_with_size` | `SetFilePointerEx` + `SetEndOfFile` | `ERROR_INVALID_PARAMETER`, `ERROR_DISK_FULL` | Closes handle manually before returning error |
| `FileWriter::sync` | `FlushFileBuffers` | `ERROR_ACCESS_DENIED` (read-only), `ERROR_INVALID_HANDLE` | Returns `io::Error::last_os_error()` |
| `FileWriter::preallocate` | `SetFilePointerEx` + `SetEndOfFile` + `SetFilePointerEx` (restore) | `ERROR_DISK_FULL`, `ERROR_INVALID_PARAMETER` | Returns error; file pointer position may be inconsistent on second `SetFilePointerEx` failure |
| `IocpWriter::drop` | `flush_buffer` + `CloseHandle` | Flush failure | Errors swallowed (`let _ = ...`) |

### 1.3 file_reader.rs

| Operation | Windows API | Possible Errors | Current Handling |
|-----------|-------------|-----------------|------------------|
| `IocpReader::open` | `CreateFileW` | `ERROR_FILE_NOT_FOUND`, `ERROR_ACCESS_DENIED`, `ERROR_SHARING_VIOLATION` | Returns `io::Error::last_os_error()` when `INVALID_HANDLE_VALUE` |
| Metadata probe | `File::metadata()` via `FromRawHandle` + `mem::forget` | I/O errors | Propagated via `?` |
| `read_at` | `ReadFile` | `ERROR_IO_PENDING`, `ERROR_INVALID_PARAMETER`, `ERROR_HANDLE_EOF` | `ERROR_IO_PENDING` enters completion wait; code 87 upgraded via `classify_overlapped_error`; others propagated |
| `read_at` completion | `GetQueuedCompletionStatus` | `ERROR_HANDLE_EOF`, `ERROR_OPERATION_ABORTED`, `ERROR_ABANDONED_WAIT_0` | Classified via `classify_overlapped_error` |
| `read_all_batched` | Multiple `ReadFile` submissions | Mix of `ERROR_IO_PENDING` and synchronous errors | Non-pending errors returned immediately; pending ops drained via per-batch completion wait |
| `read_all_batched` completion ordering | `GetQueuedCompletionStatus` | Completions processed in submission order | Correct only because each batch waits for all ops before moving on; OOO completions within a batch are mapped back by positional index |
| `IocpReader::drop` | `CloseHandle` | `ERROR_INVALID_HANDLE` (double-close) | Return value ignored |

### 1.4 pump.rs (CompletionPump)

| Operation | Windows API | Possible Errors | Current Handling |
|-----------|-------------|-----------------|------------------|
| `CompletionPump::with_config` | `CompletionPort::new` + thread spawn | Port creation failure, spawn failure | Both propagated via `io::Result` |
| `associate_handle` | `CompletionPort::associate` | `ERROR_INVALID_PARAMETER`, reserved key `usize::MAX` | Reserved key returns `InvalidInput`; port errors propagated |
| `register` | Mutex lock | Poisoned mutex | Panics via `.expect()` |
| `unregister` | Mutex lock | Poisoned mutex | Panics via `.expect()` |
| `drain_loop` | `GetQueuedCompletionStatusEx` | `WAIT_TIMEOUT`, `ERROR_ABANDONED_WAIT_0`, `ERROR_INSUFFICIENT_BUFFER`, `ERROR_INVALID_PARAMETER`, any other | `WAIT_TIMEOUT` - continue; `ERROR_ABANDONED_WAIT_0` - break (graceful); `ERROR_INSUFFICIENT_BUFFER` - grow buffer up to `MAX_BATCH_SIZE`; at cap - return `IocpError::InsufficientBuffer`; others - return classified error |
| `drain_loop` completion dispatch | OVERLAPPED Internal field | Non-zero NTSTATUS | Translated via `ntstatus_to_dos_error`; `ERROR_HANDLE_EOF` mapped to `UnexpectedEof`; others to `io::Error::from_raw_os_error` |
| `shutdown` | `PostQueuedCompletionStatus` | Post failure (port closed) | Return value ignored (non-fatal; timeout fallback) |
| `post_completion` | `PostQueuedCompletionStatus` | Port closed, invalid handle | Returns `io::Error::last_os_error()` |
| Handler dispatch | Lookup by OVERLAPPED address | No handler found (unregistered) | Silently dropped |
| Null OVERLAPPED entry | Spurious completion | No handler, null pointer | Skipped via `continue` |

### 1.5 socket.rs

| Operation | Windows API | Possible Errors | Current Handling |
|-----------|-------------|-----------------|------------------|
| `IocpSocketReader::associate` | `CompletionPump::associate_handle` | `WSAEINVAL` (socket not overlapped) | Propagated via `io::Result` |
| `recv_async` | `WSARecv` | `WSA_IO_PENDING` (expected), `WSAECONNRESET`, `WSAEDISCON`, `WSAESHUTDOWN`, `WSAENETRESET`, `WSAECONNABORTED` | `WSA_IO_PENDING` enters completion wait; graceful-close codes mapped to `Ok(0)` via `map_recv_error`; others propagated |
| `recv_async` completion | Channel recv | Pump worker exited | Returns `io::Error::other("iocp pump worker exited before completion")` |
| `recv_async` completion | Result from pump | `UnexpectedEof` (STATUS_END_OF_FILE) | Mapped to `Ok(0)` (EOF semantic matching upstream `safe_read`) |
| `send_async` | `WSASend` | `WSA_IO_PENDING`, `WSAESHUTDOWN`, `WSAECONNRESET`, `WSAECONNABORTED` | `WSA_IO_PENDING` enters wait; broken-pipe codes mapped to `BrokenPipe` kind; others propagated |
| `send_async` completion | Channel recv | Pump worker exited | Returns `io::Error::other(...)` |

### 1.6 transmit_file.rs (feature-gated: `transmitfile`)

| Operation | Windows API | Possible Errors | Current Handling |
|-----------|-------------|-----------------|------------------|
| `try_transmit_file` | `TransmitFile` | `ERROR_NOT_SUPPORTED` (SMB/DFS/encrypted), broken pipe, connection reset | `ERROR_NOT_SUPPORTED` mapped to `Unsupported` kind; others propagated as-is |
| Length validation | (pre-check) | `length > u32::MAX` | Returns `InvalidInput` before any FFI call |
| Zero-length | (short-circuit) | None | Returns `Ok(0)` immediately |

### 1.7 disk_batch/ (IocpDiskBatch)

| Operation | Windows API | Possible Errors | Current Handling |
|-----------|-------------|-----------------|------------------|
| `IocpDiskBatch::new` | `CompletionPort::new(1)` | Memory/resource exhaustion | Propagated |
| `begin_file` | `ReOpenFile` (via `reopen_overlapped`) | `ERROR_SHARING_VIOLATION`, `ERROR_ACCESS_DENIED`, `ERROR_INVALID_PARAMETER` | Error propagated; overlapped handle closed on association failure |
| `begin_file` | `CompletionPort::associate` | `ERROR_INVALID_PARAMETER` | Closes the just-reopened handle, then propagates error |
| `write_data` | (no active file) | Logical error | Returns `InvalidInput` |
| `flush_current` / `submit_write_batch` | `WriteFile` (per chunk) | `ERROR_IO_PENDING` (expected), `ERROR_DISK_FULL`, `ERROR_INVALID_PARAMETER`, any synchronous error | `ERROR_IO_PENDING` - normal async path; others return as fatal, crediting partial progress |
| `flush_current` / `drain_completions` | `GetQueuedCompletionStatusEx` | `WAIT_TIMEOUT` (retried), non-zero NTSTATUS in OVERLAPPED | `WAIT_TIMEOUT` loops; NTSTATUS translated to Win32 error; `ERROR_HANDLE_EOF` mapped to `UnexpectedEof` |
| Zero-byte completion | (drain result) | Overlapped write returned 0 bytes | Returns `WriteZero` error |
| Short write | (drain result) | Transferred < chunk length | Remainder resubmitted at adjusted offset |
| `commit_file` (fsync) | `FlushFileBuffers` | `ERROR_ACCESS_DENIED`, `ERROR_INVALID_HANDLE` | Closes overlapped handle, propagates error |
| Fault injection | (test hook) | `take_injected_write_error` | Returns synthetic `io::Error::from_raw_os_error` before `WriteFile` |
| `IocpDiskBatch::drop` | `flush_current` | Any flush error | Swallowed (`let _ = ...`) |

### 1.8 error.rs (typed error classification)

| Function | Input | Output |
|----------|-------|--------|
| `classify_overlapped_error` | `io::Error` with code 87 | `IocpError::InvalidOperation` (mapped to `InvalidInput` kind) |
| `classify_overlapped_error` | Any other error | Pass-through |
| `is_invalid_parameter` | OS error code | `true` when code == 87 |
| `is_insufficient_buffer` | OS error code | `true` when code == 122 |

### 1.9 file_factory.rs

| Operation | Windows API | Possible Errors | Current Handling |
|-----------|-------------|-----------------|------------------|
| `writer_from_file` (Enabled) | `GetFinalPathNameByHandleW` | Anonymous handle (pipe, unnamed) | Returns `Unsupported` with descriptive message |
| `writer_from_file` (Enabled) | `IocpWriter::create_for_append` (reopen) | `ERROR_ACCESS_DENIED`, unsupported object type | Returns `Unsupported` with path info |
| `writer_from_file` (Auto) | Same as above | All failures | Transparent fallback to `StdFileWriter` |
| `reader_from_path` (Enabled) | `IocpReader::open` | All `IocpReader::open` errors | Propagated |
| Factory `open`/`create` | `IocpReader::open` / `IocpWriter::create` | Any | Silently falls back to Std variant on error |

### 1.10 overlapped.rs

| Operation | Possible Issues | Current Handling |
|-----------|-----------------|------------------|
| `as_overlapped_ptr` | Must remain pinned during async op | Enforced via `Pin<Box<Self>>` |
| `buffer_ptr` | Buffer must outlive kernel reference | Same pin guarantee |
| Page-aligned variant | `PageAlignedBuffer::new(0)` edge case | `.max(1)` prevents zero-size allocation |

---

## 2. Error Severity Classification

### 2.1 Fatal (require abort)

| Error | Source | Impact |
|-------|--------|--------|
| `CreateIoCompletionPort` returns null | `CompletionPort::new` | No completion port - cannot use IOCP at all |
| `CreateFileW` returns `INVALID_HANDLE_VALUE` | Reader/Writer open | Cannot access target file |
| `ReOpenFile` returns `INVALID_HANDLE_VALUE` | `reopen_overlapped` in disk_batch | Cannot convert to overlapped handle |
| `CompletionPort::associate` fails | Any handle association | Handle will never receive completions |
| `WriteFile` with error other than `ERROR_IO_PENDING` | Writer submit | Data loss - write did not reach kernel |
| `GetQueuedCompletionStatus` failure (non-timeout) | Completion wait | Cannot determine if write succeeded |
| `ERROR_DISK_FULL` (code 112) | `WriteFile` submission | File system full - cannot write |
| `ERROR_HANDLE_DISK_FULL` (code 39) | `WriteFile` submission | Handle-level full |
| `NTSTATUS` failure in OVERLAPPED.Internal | Completion drain | Kernel reports operation failed |
| `FlushFileBuffers` failure | fsync path | Data may not be durable |
| Mutex poisoned | Pump handler registry | Unrecoverable - another thread panicked |
| Pump worker thread panic | `JoinHandle::join` | Unwound via `expect` - propagates panic |
| `ERROR_INSUFFICIENT_BUFFER` at `MAX_BATCH_SIZE` | Pump drain loop | Runaway producer overwhelming the port |

### 2.2 Recoverable (retry or fallback)

| Error | Source | Recovery |
|-------|--------|----------|
| `ERROR_IO_PENDING` (code 997) | `WriteFile` / `ReadFile` / `WSARecv` / `WSASend` | Normal async path - wait on completion port |
| `WAIT_TIMEOUT` | `GetQueuedCompletionStatusEx` | Loop and re-check running flag |
| `ERROR_INSUFFICIENT_BUFFER` (below cap) | Pump `drain_loop` | Double the entries buffer, retry |
| Short write (transferred < requested) | Completion drain in `disk_batch` | Resubmit unwritten tail at adjusted offset |
| `SetFileCompletionNotificationModes` failure | Writer open | Non-fatal; writer handles inline completions |
| Factory open/create failure | `IocpReaderFactory` / `IocpWriterFactory` | Fall back to `Std` variant |
| `writer_from_file` path recovery failure (Auto) | `GetFinalPathNameByHandleW` | Fall back to `StdFileWriter` |
| `WSA_IO_PENDING` | Socket recv/send | Normal async completion via pump |
| `WSAECONNRESET` / `WSAEDISCON` / `WSAESHUTDOWN` | Socket recv | Mapped to `Ok(0)` (graceful EOF) |
| `WSAESHUTDOWN` / `WSAECONNRESET` / `WSAECONNABORTED` | Socket send | Mapped to `BrokenPipe` |
| `ERROR_NOT_SUPPORTED` | `TransmitFile` | Caller falls back to `WSASend` loop |
| `ERROR_ABANDONED_WAIT_0` | Port closed during wait | Break drain loop (graceful shutdown) |
| `PostQueuedCompletionStatus` failure on shutdown | Pump shutdown | Non-fatal; timeout wakes worker |
| Pump worker exit before recv | Socket `await_completion` | Returns descriptive error; caller retries at higher level |

### 2.3 Ignorable

| Error | Source | Reason |
|-------|--------|--------|
| `CloseHandle` failure in Drop impls | `CompletionPort::drop`, `IocpWriter::drop`, `IocpReader::drop` | Double-close or invalid handle during cleanup |
| Flush failure in `IocpWriter::drop` | Drop impl | Best-effort; consumer no longer interested |
| Flush failure in `IocpDiskBatch::drop` | Drop impl | Best-effort cleanup |
| Unregistered OVERLAPPED completion in pump | Handler not found | Caller submitted raw I/O without registering |
| Null OVERLAPPED in completion entry | Spurious kernel notification | Skipped via null check |
| `PostQueuedCompletionStatus` failure during shutdown | Shutdown sentinel | Timeout fallback ensures worker exits |

---

## 3. Test Coverage Analysis

### 3.1 Covered Error Paths

| Error Path | Test Location |
|------------|---------------|
| `IocpError::InvalidOperation` construction and kind | `error.rs::tests::invalid_operation_into_io_error_uses_invalid_input_kind` |
| `IocpError::InsufficientBuffer` construction and kind | `error.rs::tests::insufficient_buffer_into_io_error_uses_oom_kind` |
| `is_invalid_parameter` for code 87 | `error.rs::tests::is_invalid_parameter_recognises_code_87` |
| `is_insufficient_buffer` for code 122 | `error.rs::tests::is_insufficient_buffer_recognises_code_122` |
| `classify_overlapped_error` upgrade for code 87 | `error.rs::tests::classify_upgrades_invalid_parameter` |
| `classify_overlapped_error` pass-through for other codes | `error.rs::tests::classify_passes_through_other_errors` |
| Pump creates and shuts down cleanly | `pump.rs::tests::pump_creates_and_shuts_down` |
| Pump rejects reserved key `usize::MAX` | `pump.rs::tests::pump_rejects_reserved_key` |
| Pump register/unregister | `pump.rs::tests::pump_unregister_returns_handler_present` |
| Pump dispatches real overlapped write | `pump.rs::tests::pump_dispatches_overlapped_write_completion` |
| Pump `post_completion` round-trip | `pump.rs::tests::pump_post_completion_round_trip` |
| Pump burst larger than batch size (implicit buffer growth) | `pump.rs::tests::pump_drains_burst_larger_than_batch_size` |
| `InsufficientBuffer` error round-trip | `pump.rs::tests::iocp_error_insufficient_buffer_round_trips` |
| `IocpDiskBatch` write without active file | `disk_batch/tests.rs::write_without_active_file_errors` |
| `IocpDiskBatch` commit without active file | `disk_batch/tests.rs::commit_without_active_file_errors` |
| `IocpDiskBatch` `ReOpenFile` failure (read-only handle) | `disk_batch/tests.rs::error_propagates_when_reopen_overlapped_fails` |
| `ERROR_DISK_FULL` injected first submission | `tests/iocp_disk_full_simulation.rs::first_submission_disk_full_surfaces_storage_full` |
| Drop after `ERROR_DISK_FULL` is clean | `tests/iocp_disk_full_simulation.rs::writer_drop_after_disk_full_is_clean` |
| Recovery after injected fault consumed | `tests/iocp_disk_full_simulation.rs::batch_recovers_after_injected_fault_consumed` |
| Nth-submission fault injection | `tests/iocp_disk_full_simulation.rs::nth_submission_disk_full_skips_earlier_writes` |
| Win32 `ERROR_DISK_FULL` kind mapping | `tests/iocp_disk_full_simulation.rs::std_maps_disk_full_codes_to_storage_full_kind` |
| Partial-write accumulation (disk batch) | `tests/iocp_partial_write_integration.rs::disk_batch_accumulates_under_simulated_pressure` |
| Partial-write accumulation (IocpWriter) | `tests/iocp_partial_write_integration.rs::writer_accumulates_across_implicit_flushes` |
| Begin file with read-only handle | `tests/iocp_partial_write_integration.rs::disk_batch_begin_file_with_readonly_handle_errors` |
| Write/commit without active file | `tests/iocp_partial_write_integration.rs::disk_batch_write_without_active_file_returns_invalid_input` |
| `SeekFrom::End` unsupported | `tests/iocp_completion_port_integration.rs::writer_seek_end_returns_error` |
| Reader `seek_to` past EOF | `tests/iocp_completion_port_integration.rs::reader_seek_beyond_eof_errors` |
| Socket peer-shutdown returns `Ok(0)` | `socket.rs::tests::recv_after_peer_shutdown_returns_eof` |
| Empty recv/send buffer returns 0 without I/O | `socket.rs::tests::empty_recv_buffer_returns_zero_without_io`, `empty_send_buffer_returns_zero_without_io` |
| Factory forced fallback | `file_factory.rs::tests::factory_reader_forced_fallback`, `factory_writer_forced_fallback` |
| `writer_from_file` rejects anonymous handle (Enabled) | `file_factory.rs::tests::writer_from_file_enabled_rejects_anonymous_handle` |
| `writer_from_file` auto falls back for anonymous handle | `file_factory.rs::tests::writer_from_file_auto_falls_back_for_anonymous_handle` |
| `TransmitFile` length overflow rejected | `transmit_file.rs::tests::transmit_file_rejects_oversized_length` |
| `TransmitFile` against non-socket | `transmit_file.rs::tests::transmit_file_non_socket_target_returns_error` |
| `TransmitFile` zero-length noop | `transmit_file.rs::tests::transmit_file_zero_length_is_noop` |
| High-concurrency 10K files (stress-gated) | `tests/iocp_high_concurrency_stress.rs` (both tests) |
| Completion ordering independent of submission | `disk_batch/tests.rs::completion_ordering_independent_of_submission_order` |

### 3.2 Untested Error Conditions

| Error Condition | File | Risk | Notes |
|-----------------|------|------|-------|
| **`CreateIoCompletionPort` failure in `CompletionPort::new`** | `completion_port.rs` | Low | Requires kernel resource exhaustion; tested implicitly (if it fails, every subsequent test fails) |
| **`CompletionPort::associate` failure (non-overlapped handle)** | `completion_port.rs` | Medium | No unit test passes a non-overlapped handle directly to `associate`; covered indirectly by disk_batch readonly test |
| **`GetQueuedCompletionStatus` returns error (non-pending)** | `file_writer.rs:211`, `file_reader.rs:149` | Medium | No test drives a real completion failure through the per-file writer/reader path; only tested via the disk_batch drain loop |
| **`FlushFileBuffers` failure** | `file_writer.rs:323` | Medium | Never exercised; would require a read-only handle reaching the sync path |
| **`SetFilePointerEx` failure in `preallocate` (second call)** | `file_writer.rs:343` | Low | Tested for the success path only; failure leaves writer in inconsistent file-pointer state |
| **`ERROR_OPERATION_ABORTED` (NTSTATUS `STATUS_CANCELLED`)** | `pump.rs` drain loop | Medium | `ntstatus_to_dos_error` maps it to 995 but no test verifies a cancelled operation propagates correctly to the handler |
| **`STATUS_INSUFFICIENT_RESOURCES` (NTSTATUS 0xC000009A)** | `pump.rs` drain loop | Medium | Mapped to Win32 1450 but never exercised |
| **`STATUS_IO_TIMEOUT` (NTSTATUS 0xC00000B5)** | `pump.rs` drain loop | Low | Mapped to 121 but no test exercises an overlapped timeout |
| **`ERROR_ABANDONED_WAIT_0` during pump drain** | `pump.rs:384` | Low | Handled as graceful break; no test closes the port while the pump is running |
| **`ERROR_INSUFFICIENT_BUFFER` at `MAX_BATCH_SIZE`** | `pump.rs:397` | Low | The synthetic test only verifies the typed error round-trips; no test actually hits this from `GetQueuedCompletionStatusEx` |
| **Pump worker thread spawn failure** | `pump.rs:207` | Low | Requires resource exhaustion |
| **Mutex poisoning in pump** | `pump.rs:259,269,280` | Low | Only occurs if another thread panics while holding the lock |
| **Socket `WSARecv` synchronous failure (non-pending, non-graceful)** | `socket.rs:205-207` | Medium | No test drives a real synchronous WSARecv failure (e.g., `WSAEFAULT`) |
| **Socket `WSASend` synchronous failure (non-pending, non-broken-pipe)** | `socket.rs:329-331` | Medium | No test drives `WSAENETDOWN` or similar |
| **Pump worker exits before socket completion arrives** | `socket.rs:429-431` | Medium | The `io::Error::other("iocp pump worker exited")` path has no dedicated test |
| **`TransmitFile` failure with a real `ERROR_NOT_SUPPORTED`** | `transmit_file.rs` | Low | Requires SMB/DFS volume; test uses non-socket for error, not `Unsupported` kind specifically |
| **`IocpWriter::write_at` zero-byte write (WriteFile returns 0 synchronously)** | `file_writer.rs:181` | Low | Empty writes short-circuit; zero-return from a non-empty request has no dedicated test |
| **Out-of-order completion delivery in `read_all_batched`** | `file_reader.rs:218-249` | High | Completions are processed by positional index, not by matching OVERLAPPED pointers; reordering within a batch would corrupt data |
| **`GetFinalPathNameByHandleW` path changes between calls** | `file_factory.rs:386-419` | Very Low | Race condition on shared volumes; defensive buffer sizing handles it |
| **`IocpDiskBatch` `drain_completions` timeout with `DRAIN_TIMEOUT_MS = u32::MAX`** | `disk_batch/completion.rs:135` | Low | Infinite wait; if `WriteFile` queues but never completes (e.g., disk removed), the thread hangs forever |

---

## 4. Race Conditions and Async Completion Hazards

### 4.1 Identified Race Conditions

| Race | Location | Severity | Mitigation |
|------|----------|----------|------------|
| **Batch read completion ordering** | `file_reader.rs:218-249` | **High** | Completions within a batch are mapped by index, not OVERLAPPED address. If Windows delivers CQEs out of order within a single `GetQueuedCompletionStatus` sequence, the wrong data is written to the wrong buffer offset. Mitigated only by the serial `GetQueuedCompletionStatus` loop (one at a time), but the kernel is not contractually obligated to deliver them in submission order. |
| **OVERLAPPED lifetime during pump dispatch** | `pump.rs:414-444` | Low | The pump reads `(*entry.lpOverlapped).Internal` after dequeue. The pinned op must still be alive. Correct: the handler is removed from the registry first, then invoked, and the op is dropped only after the handler returns. |
| **Socket overlapped lifetime** | `socket.rs:159-210` | Low | The `Box<OVERLAPPED>` is kept alive through `await_completion` which borrows it. Correct: the borrow prevents the Box from being freed before `rx.recv()` returns. |
| **Shutdown vs. in-flight completions** | `pump.rs:299-333` | Low | `shutdown_impl` sets `running = false` and posts a sentinel. In-flight handlers registered before shutdown fires will still be dispatched (the sentinel is just another entry in the queue). Handlers registered after `running = false` may never fire if no operations are submitted. |
| **Global fault-injection state** | `disk_batch/completion.rs:46-52` | Low (test-only) | Static atomics are process-wide. Parallel tests using the hook could interfere. Mitigated by `InjectionGuard` RAII pattern and single-shot semantics. |

### 4.2 Potential Undefined Behavior Risks

| Risk | Location | Assessment |
|------|----------|------------|
| **`CompletionPort` Send+Sync impls** | `completion_port.rs:80-86` | Sound. Windows kernel objects are thread-safe by definition. The manual impls are justified. |
| **`IocpWriter` Send impl** | `file_writer.rs:366` | Sound for single-threaded use. The writer holds an exclusive HANDLE and is not Sync. Sending it to another thread is safe because HANDLE is a kernel object. |
| **`OverlappedOp` pin projection** | `overlapped.rs:183-186` | Sound. `get_unchecked_mut` on a `Pin<Box<>>` is safe when only the overlapped field is accessed (structural pin). The buffer pointer is also stable because it is heap-allocated within the Box. |
| **`IocpReader::open` FromRawHandle + mem::forget** | `file_reader.rs:67-76` | Sound but fragile. Creates a `File` from the raw handle to call `.metadata()`, then forgets it to avoid double-close. A panic between `from_raw_handle` and `mem::forget` would double-close. In practice, `metadata()` is unlikely to panic, but the pattern could be replaced by direct `GetFileSizeEx` for safety. |
| **OVERLAPPED union access** | `disk_batch/writer.rs:199-204`, `overlapped.rs:229-231` | Sound. The union fields `Offset`/`OffsetHigh` are plain u32 values set by the writer and read after kernel return. No type punning. |

---

## 5. Comparison with io_uring Error Handling

### 5.1 Structural Differences

| Aspect | IOCP | io_uring |
|--------|------|----------|
| Error delivery | NTSTATUS in `OVERLAPPED.Internal`, translated to Win32 | CQE `res` field: negative errno |
| Pending indicator | `ERROR_IO_PENDING` (997) from submit call | No equivalent - all submissions are async by design |
| Completion batching | `GetQueuedCompletionStatusEx` with dynamic buffer growth | `io_uring_peek_batch_cqe` / `io_uring_wait_cqe` |
| Handle lifecycle | Explicit `ReOpenFile` + `CloseHandle` | fd registration via `IORING_REGISTER_FILES`; unregister to release |
| Disk-full detection | `ERROR_DISK_FULL` (112) from `WriteFile` or NTSTATUS | `-ENOSPC` in CQE res |
| Cancellation | `CancelIoEx` (not yet wired) | `IORING_OP_ASYNC_CANCEL` with linked SQEs |
| Timeout | Hardcoded `u32::MAX` in disk batch, 100ms in pump | `IORING_OP_LINK_TIMEOUT` for per-op timeout |

### 5.2 Asymmetries Where One Platform Handles Better

| Scenario | Winner | Explanation |
|----------|--------|-------------|
| **Per-operation timeout** | io_uring | io_uring supports linked timeout SQEs; IOCP disk_batch waits indefinitely (`DRAIN_TIMEOUT_MS = u32::MAX`) - a hung write can block the commit thread forever |
| **Cancellation** | io_uring | io_uring has `IORING_OP_ASYNC_CANCEL`; IOCP has `CancelIoEx` but it is not wired into the current code |
| **Completion ordering guarantee** | IOCP (pump) | The pump matches by OVERLAPPED address, so reordering is handled correctly. The `IocpReader::read_all_batched` assumes order - this is WORSE than io_uring which matches by user_data |
| **Buffer growth under pressure** | IOCP (pump) | The pump dynamically grows its drain buffer on `ERROR_INSUFFICIENT_BUFFER`. io_uring CQ is fixed-size and overflows cause `CQ_OVERFLOW` requiring a `io_uring_enter` retry. Both handle it, but the IOCP approach is simpler. |
| **Typed error classification** | IOCP | `IocpError::InvalidOperation` / `InsufficientBuffer` provide actionable messages. io_uring errors are raw negative errno values without typed wrappers. |
| **Graceful socket close mapping** | Equivalent | Both map peer-close to `Ok(0)`. IOCP handles 5 Winsock codes; io_uring handles `ECONNRESET`/`EPIPE`. |
| **Resource leak prevention** | io_uring (slight edge) | io_uring's ring cleanup automatically cancels pending ops on `close()`. IOCP relies on `CloseHandle` + drain, but pending completions for closed handles are silently discarded by the kernel. |
| **Short-write resubmission** | Equivalent | Both `IocpDiskBatch` and `IoUringDiskBatch` handle short writes by resubmitting the tail. |
| **Fault injection for testing** | IOCP | The IOCP path has an explicit `inject_next_write_error_for_test` hook. The io_uring path has no equivalent; tests rely on real kernel behavior or mock rings. |

### 5.3 Missing Parity Items

| Gap | Platform | Impact |
|-----|----------|--------|
| No `CancelIoEx` integration | IOCP | Cannot cancel in-flight operations on shutdown; must wait for kernel completion or timeout |
| No per-op timeout in disk_batch | IOCP | A single hung write blocks the commit thread indefinitely |
| No linked-chain equivalent | IOCP | io_uring `linked_chain.rs` provides dependent-op sequencing; IOCP has no equivalent |
| No fixed-buffer registration | IOCP | io_uring `IORING_REGISTER_BUFFERS` eliminates per-op buffer pinning overhead; IOCP allocates per-op |
| No `SQPOLL` equivalent | IOCP | io_uring kernel-side polling eliminates the drain syscall; IOCP always requires `GetQueuedCompletionStatusEx` |

---

## 6. Recommendations

### 6.1 High Priority

1. **Fix `read_all_batched` completion ordering (HIGH)**: The reader assumes
   completions arrive in submission order. Match completions to their
   OVERLAPPED addresses (as the pump does) to prevent data corruption if
   the kernel reorders CQEs within a single drain call.

2. **Add per-operation timeout to `disk_batch`**: The `DRAIN_TIMEOUT_MS =
   u32::MAX` means a stuck disk blocks the commit thread forever. Add a
   configurable timeout (e.g., 30 seconds) with an `io::ErrorKind::TimedOut`
   return, matching io_uring's linked-timeout capability.

3. **Wire `CancelIoEx` for graceful shutdown**: On `IocpDiskBatch::drop` or
   pump shutdown, cancel pending operations rather than waiting for them
   to complete naturally. Prevents shutdown hangs when I/O is stuck on a
   slow device.

### 6.2 Medium Priority

4. **Test `GetQueuedCompletionStatus` failure in per-file writer/reader**:
   The code paths at `file_writer.rs:211` and `file_reader.rs:149` are
   only tested indirectly. Add a test that closes the underlying HANDLE
   while a read/write is pending to exercise `ERROR_OPERATION_ABORTED`.

5. **Test pump worker exit before socket completion**: Shut down the pump
   while a socket `recv_async` is blocked to verify the "worker exited"
   error path.

6. **Replace `FromRawHandle` + `mem::forget` in IocpReader::open**: Use
   `GetFileSizeEx` directly instead of the fragile pattern that would
   double-close on panic.

7. **Add NTSTATUS translation tests**: Write unit tests for each mapped
   NTSTATUS code (`STATUS_CANCELLED`, `STATUS_INSUFFICIENT_RESOURCES`,
   `STATUS_IO_TIMEOUT`) to verify the `ntstatus_to_dos_error` function.

### 6.3 Low Priority

8. **Test `ERROR_ABANDONED_WAIT_0`**: Close the completion port from another
   thread while the pump drain loop is waiting to verify the graceful-break
   path.

9. **Document the `preallocate` file-pointer inconsistency**: If the third
   `SetFilePointerEx` call fails in `preallocate`, the file pointer is at
   the end of the file rather than the expected logical offset. Consider
   adding an error-path restore or documenting the invariant for callers.

10. **Add `ERROR_NOT_SUPPORTED` test for `TransmitFile`**: This requires a
    non-NTFS volume or mock. The current non-socket test exercises a
    different error code.

---

## 7. Summary

The IOCP error handling is generally well-structured:

- **Typed errors** (`IocpError`) provide actionable messages for the two
  most common Windows misuse patterns.
- **Fault injection** enables deterministic testing of `ERROR_DISK_FULL`
  without real disk exhaustion.
- **Graceful degradation** through factory fallback to standard I/O.
- **Short-write resubmission** matches io_uring parity.

The primary gaps are:

- **`read_all_batched` order assumption** - a correctness risk if the
  kernel reorders completions within a batch drain.
- **No per-op timeout in disk_batch** - a liveness risk on degraded storage.
- **No cancellation** - `CancelIoEx` is not wired, so shutdown can hang.
- **NTSTATUS translation** is minimal (4 codes) - unmapped statuses pass
  through as opaque values that `io::Error` cannot classify.

Total unique error paths: ~45. Test-covered paths: ~32 (71%). The 14
untested paths concentrate in completion-failure scenarios that are
difficult to reproduce without kernel cooperation or purpose-built mocks.
