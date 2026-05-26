# WIN-G.c: Windows GNU Exception-Handling ABI Compatibility Status

Audit of the `windows-gnu-eh` crate, its role in the oc-rsync dependency tree,
and the broader implications of EH ABI choice for Windows FFI boundaries.

## 1. Dependency Analysis

### What windows-gnu-eh provides

The `crates/windows-gnu-eh` crate supplies two `#[no_mangle]` C-ABI shim
functions - `___register_frame_info` and `___deregister_frame_info` - that
forward to the modern two-underscore variants (`__register_frame_info` /
`__deregister_frame_info`) at runtime.

When cross-compiling for `x86_64-pc-windows-gnu` with `cargo-zigbuild`, the
Zig-provided toolchain omits the legacy three-underscore libgcc entry points.
Rust's startup object `rsbegin.o` references them whenever DWARF unwind data
is present, causing link failures. The shims resolve the modern symbols from
whichever libgcc/libunwind DLL is loaded (checked in order:
`libgcc_s_seh-1.dll`, `libgcc_s_sjlj-1.dll`, `libgcc_s_dw2-1.dll`,
`libunwind.dll`) via `LoadLibraryA`/`GetProcAddress`, and cache them in
`AtomicUsize` statics. If no provider is found, the shims silently no-op -
safe because the symbols are only exercised when DWARF unwinding is active.

### Which crate depends on it

Only the root binary (`src/bin/oc-rsync.rs`) depends on `windows-gnu-eh`.
The dependency is conditional:

```toml
# Cargo.toml (workspace root)
[target.'cfg(all(windows, target_env = "gnu"))'.dependencies]
windows-gnu-eh = { path = "crates/windows-gnu-eh" }
```

The binary calls `windows_gnu_eh::force_link()` behind a matching cfg gate:

```rust
#[cfg(all(target_os = "windows", target_env = "gnu"))]
windows_gnu_eh::force_link();
```

On all non-GNU targets the crate compiles to a zero-cost no-op.

### Why it exists

The crate exists solely to unblock cross-compilation from Linux to
`x86_64-pc-windows-gnu` using Zig as the C linker. Without it, the link
step fails with unresolved symbols for the triple-underscore frame
registration entry points. No library crate in the workspace depends on it.

## 2. Target Triple Inventory

### CI build matrix

| Job | Runner | Target Triple | What runs |
|-----|--------|---------------|-----------|
| `windows-test` | `windows-latest` | `x86_64-pc-windows-msvc` (implicit) | `cargo nextest run -p core -p engine -p cli --all-features` |
| `windows-iocp` | `windows-latest` | `x86_64-pc-windows-msvc` (implicit) | `cargo nextest run -p fast_io -p transfer --features iocp` |
| `windows-acl-xattr` | `windows-latest` | `x86_64-pc-windows-msvc` (implicit) | DACL round-trip and xattr ADS tests |
| `windows-gnu-cross-check` | `ubuntu-latest` | `x86_64-pc-windows-gnu` (explicit) | `cargo check --workspace` (compile only, no tests) |

### Release build matrix

| Target Triple | Toolchain variants | Artifact format |
|---------------|--------------------|-----------------|
| `x86_64-pc-windows-msvc` | stable, beta, nightly | `.tar.gz` + `.zip` |

The GNU target is never tested at runtime in CI and never shipped in release
artifacts. All Windows release binaries are MSVC.

### Summary

- **MSVC**: fully tested (three CI jobs) and shipped
- **GNU**: compile-checked only; no runtime tests, no release artifacts

## 3. FFI Boundary Audit

All Windows FFI in oc-rsync falls into two categories based on the binding
crate used:

- **`windows` crate** (microsoft/windows-rs) - high-level safe wrappers with
  `Result<T, windows::core::Error>` returns. Used by `platform` and
  `metadata` crates.
- **`windows-sys` crate** - raw `extern "system"` bindings returning integer
  status codes. Used by `fast_io/src/iocp/`.

Both binding crates produce calls with `extern "system"` ABI (stdcall on
x86, MS fastcall on x64). This is the correct ABI for Win32 APIs regardless
of whether the Rust binary is built with MSVC or GNU target environment.

### FFI call sites by crate

#### fast_io (IOCP subsystem) - uses `windows-sys`

| Module | Win32 APIs called | Panic risk |
|--------|-------------------|------------|
| `iocp/completion_port.rs` | `CreateIoCompletionPort`, `CloseHandle` | None - pure handle lifecycle |
| `iocp/pump.rs` | `GetQueuedCompletionStatusEx`, `PostQueuedCompletionStatus` | Low - drain loop is `catch_unwind`-free but runs on a dedicated thread; a panic here propagates via `JoinHandle::join().expect()` |
| `iocp/file_writer.rs` | `CreateFileW`, `WriteFile`, `SetFilePointerEx`, `SetEndOfFile`, `FlushFileBuffers`, `CloseHandle`, `GetQueuedCompletionStatus`, `SetFileInformationByHandle` | Low - each call is wrapped in `unsafe` block with immediate error checking |
| `iocp/file_reader.rs` | `CreateFileW`, `ReadFile`, `GetQueuedCompletionStatus`, `CloseHandle` | Low - same pattern as writer |
| `iocp/file_factory.rs` | `GetFinalPathNameByHandleW`, `CreatePipe`, `CloseHandle` | Low |
| `iocp/socket.rs` | `WSARecv`, `WSASend` | Low - overlapped Winsock calls with completion pump dispatch |
| `iocp/transmit_file.rs` | `TransmitFile` | Low - single call with error mapping |
| `iocp/disk_batch/completion.rs` | `GetQueuedCompletionStatusEx` | Low - batched drain |
| `iocp/disk_batch/mod.rs` | `FlushFileBuffers` | Low |
| `iocp/config.rs` | `CreateIoCompletionPort`, `CloseHandle` | Low - probe-and-discard pattern |
| `iocp/overlapped.rs` | None (struct layout only) | None |
| `copy_file_ex.rs` | `CopyFileExW` | Low |

#### platform - uses `windows` crate

| Module | Win32 APIs called | Panic risk |
|--------|-------------------|------------|
| `signal.rs` | `SetConsoleCtrlHandler` | None - registration is one-shot; the `extern "system"` callback only sets atomics |
| `windows_service.rs` | `StartServiceCtrlDispatcherW`, `RegisterServiceCtrlHandlerW`, `SetServiceStatus`, `OpenSCManagerW`, `CreateServiceW`, `OpenServiceW`, `DeleteService`, `CloseServiceHandle` | Low - SCM callbacks set atomics or report status |
| `privilege.rs` | `LogonUserW`, `ImpersonateLoggedOnUser`, `CloseHandle`, `OpenProcessToken`, `AdjustTokenPrivileges`, `LookupPrivilegeValueW` | Low |
| `name_resolution.rs` | `LookupAccountNameW`, `GetSidSubAuthority`, `GetSidSubAuthorityCount`, `NetUserEnum`, `NetApiBufferFree` | Low |

#### metadata - uses `windows` crate

| Module | Win32 APIs called | Panic risk |
|--------|-------------------|------------|
| `acl_windows/dacl.rs` | `GetSecurityInfo`, `IsValidSid`, `LookupAccountSidW`, `GetAce`, `SetNamedSecurityInfoW`, `InitializeAcl`, `AddAccessAllowedAce`, `SetSecurityDescriptorDacl` | Low |
| `acl_windows/sddl.rs` | `GetNamedSecurityInfoW`, `ConvertSecurityDescriptorToStringSecurityDescriptorW`, `ConvertStringSecurityDescriptorToSecurityDescriptorW`, `GetSecurityDescriptorOwner`, `GetSecurityDescriptorGroup` | Low |
| `acl_windows/common.rs` | `LocalFree` | Low |
| `xattr_windows.rs` | `FindFirstStreamW`, `FindNextStreamW`, `FindClose`, `CreateFileW`, `DeleteFileW` | Low |
| `copy_as.rs` | `OpenProcessToken`, `AdjustTokenPrivileges`, `LookupPrivilegeValueW`, `CloseHandle`, `GetCurrentProcess` | Low |

### Unwind safety at FFI boundaries

Every FFI call follows the same pattern:

1. Prepare arguments (UTF-16 conversion, buffer allocation) in safe Rust.
2. Enter a scoped `unsafe` block for the single Win32 call.
3. Check the return value immediately.
4. Convert errors to `io::Error` or `windows::core::Error`.

No Rust closures or user callbacks are invoked from within the FFI call
itself (with two exceptions noted below). A panic in the surrounding safe
Rust code would unwind normally without crossing an FFI boundary.

**Exception 1 - SetConsoleCtrlHandler callback**: The `extern "system" fn
handler` in `platform/src/signal.rs` is invoked by the Windows console
subsystem on an OS-created thread. The handler body only loads `OnceLock`
references and stores atomics - operations that cannot panic.

**Exception 2 - SCM callbacks**: `service_main_entry` and
`service_control_handler` in `platform/src/windows_service.rs` are
`extern "system"` functions called by the SCM dispatcher. The control
handler only touches atomics (cannot panic). The main entry calls
`OnceLock::set`, `Mutex::lock`, and the user-provided `ServiceMainCallback`.
If the callback panics, the panic would unwind through the `extern "system"`
frame - this is undefined behavior under the current Rust ABI rules (see
risk assessment below).

## 4. Release Binary Implications

Release binaries target `x86_64-pc-windows-msvc` exclusively. On this
target:

- Panic unwinding uses SEH (Structured Exception Handling), the native
  Windows mechanism.
- `extern "system"` calls unwind correctly because SEH is the OS-provided
  mechanism and MSVC-linked Rust binaries participate in the same frame
  tables.
- The `windows-gnu-eh` crate compiles to a no-op and is optimized away
  entirely.

**The crate has zero effect on shipped binaries.**

For hypothetical GNU-target builds:

- Panic unwinding uses DWARF tables (or SJLJ on older MinGW).
- The `windows-gnu-eh` crate's shims ensure DWARF frame registration
  symbols resolve at link time and forward to whichever runtime is loaded.
- Cross-ABI unwinding (DWARF unwind through an SEH `extern "system"` call
  frame) is not guaranteed to work. In practice, Win32 APIs are leaf-level
  kernel transitions that do not participate in userspace unwinding, so
  unwinding around them - but not through them - is safe.

## 5. SetConsoleCtrlHandler Audit

The `SetConsoleCtrlHandler` registration in `platform/src/signal.rs`:

```rust
unsafe extern "system" fn handler(ctrl_type: u32) -> windows::core::BOOL {
    match ctrl_type {
        x if x == CTRL_C_EVENT || x == CTRL_CLOSE_EVENT => {
            if let Some(flag) = SHUTDOWN.get() {
                flag.store(true, Ordering::Relaxed);
            }
            windows::core::BOOL(1)
        }
        x if x == CTRL_BREAK_EVENT => {
            if let Some(flag) = GRACEFUL.get() {
                flag.store(true, Ordering::Relaxed);
            }
            windows::core::BOOL(1)
        }
        _ => windows::core::BOOL(0),
    }
}
```

**EH ABI impact: none.** The handler:

- Is `extern "system"` (correct calling convention for both MSVC and GNU).
- Performs only infallible operations (`OnceLock::get` returns `Option`,
  `AtomicBool::store` cannot panic).
- Does not allocate, lock a mutex, or call any fallible Rust API.
- Returns a `BOOL` to the OS immediately.

A panic is impossible in this handler body. The EH ABI choice (SEH vs DWARF)
does not affect it.

## 6. Risk Assessment

### Panic-across-FFI risks

Since Rust 1.71 (RFC 2945), unwinding through `extern "C"` or
`extern "system"` functions is defined as aborting the process rather than
invoking undefined behavior. This means a panic in an `extern "system"`
callback would not silently corrupt the stack - it would abort.

**Affected sites:**

1. `service_main_entry` (`windows_service.rs:151`) - calls a user-provided
   `ServiceMainCallback` which could panic. Impact: process abort. This is
   acceptable behavior for a service main function.

2. IOCP `CompletionHandler` callbacks (`pump.rs`) - these are `Box<dyn
   FnOnce(io::Result<u32>) + Send>` invoked on the pump worker thread.
   They do not run inside an `extern "system"` frame (the drain loop is
   pure Rust calling `GetQueuedCompletionStatusEx` then dispatching
   handlers in safe Rust). A panic here would unwind the pump worker
   thread and surface through `JoinHandle::join()`.

3. Console ctrl handler (`signal.rs:124`) - infallible, cannot panic.

4. SCM control handler (`windows_service.rs:200`) - only stores atomics
   and calls `report_status_raw`. The `SetServiceStatus` call is
   infallible from the Rust side (errors mapped to `Result`). Cannot
   panic.

### IOCP completion and state corruption

The IOCP pump worker thread in `fast_io/src/iocp/pump.rs`:

- Runs a `drain_loop` function that calls `GetQueuedCompletionStatusEx`
  (FFI), then looks up and invokes `CompletionHandler` callbacks (safe
  Rust).
- If a handler panics, the panic unwinds through the drain loop on the
  worker thread. `CompletionPump::shutdown_impl` calls
  `handle.join().expect("iocp pump worker panicked")` - this propagates
  the panic to the owning thread.
- Pending OVERLAPPED operations in flight when the pump panics would
  have their handles closed by the `CompletionPort` `Drop` impl, which
  cancels pending I/O via `CloseHandle`. No state corruption occurs
  because OVERLAPPED structures are allocated on the heap and freed by
  their owning `IocpWriter`/`IocpReader` Drop impls.

**No known state corruption paths exist.** The worst case is process abort
from a panic in `service_main_entry`, which is the correct behavior for
an unrecoverable service failure.

### GNU-specific risks

Since the GNU target is compile-checked only (no tests, no release
artifacts), the practical risk surface is zero for shipped binaries.
For developers who might build with `x86_64-pc-windows-gnu`:

- DWARF unwinding through Win32 API call frames is not formally
  guaranteed, but Win32 APIs are kernel transitions that do not push
  userspace unwind frames. In practice, DWARF unwinding around Win32
  calls works correctly.
- The `windows-gnu-eh` shims provide frame registration so the DWARF
  unwinder has valid tables. Without the shims, the binary would fail
  to link - there is no silent miscompilation risk.
- `panic=abort` would eliminate all unwinding concerns for GNU builds
  but is not currently configured in the workspace profile.

## 7. Recommendations

### Keep windows-gnu-eh (no action required)

The crate should be retained as-is. Rationale:

1. **Zero cost on shipped targets.** On `x86_64-pc-windows-msvc` and all
   non-Windows targets, the crate compiles to a single no-op function
   that is optimized away. No binary size impact, no runtime overhead.

2. **Low maintenance burden.** The crate is approximately 230 lines of
   self-contained code with no external dependencies beyond `core` and
   `kernel32`. It uses stable Win32 APIs unchanged since Windows XP. No
   updates are required unless Rust changes its startup object linking
   model for the GNU target.

3. **Enables cross-compilation.** The `windows-gnu-cross-check` CI job
   validates that the entire workspace compiles for `x86_64-pc-windows-gnu`.
   This catches accidental MSVC-only dependencies early and keeps the
   option open for GNU-based cross-compilation workflows (e.g.,
   `cargo-zigbuild` from Linux).

4. **Removal criteria.** The crate can be removed if the project decides
   to drop GNU target support entirely. To remove: delete
   `crates/windows-gnu-eh/`, remove its workspace member entry, remove
   the conditional dependency from `[target.'cfg(...)'.dependencies]`,
   and remove the `force_link()` call from `src/bin/oc-rsync.rs`. The
   `windows-gnu-cross-check` CI job would also need to be removed.

### Target-triple-specific caveats

- **MSVC builds**: No EH ABI concerns. SEH unwinding works correctly
  across all FFI boundaries.
- **GNU builds**: Compile-checked only. If runtime testing is ever added,
  verify that DWARF unwinding works correctly with the specific MinGW
  runtime in use. Consider adding `panic=abort` to the GNU profile to
  eliminate unwinding uncertainty.
- **ARM64 Windows**: Not currently built or tested. If added,
  `windows-gnu-eh` would need a review - ARM64 Windows uses SEH
  exclusively (no GNU target exists in mainline Rust as of Rust 1.88).

### Future considerations

- The `extern "system" fn service_main_entry` callback should wrap its
  `ServiceMainCallback` invocation in `std::panic::catch_unwind` so a
  panicking service callback reports `SERVICE_STOPPED` with a non-zero
  exit code instead of aborting the process. This is not EH-ABI-specific
  but is good practice for any `extern` callback that runs user code.
- If the project ever adds `aarch64-pc-windows-msvc` to the release
  matrix, no changes to `windows-gnu-eh` are needed - the crate is
  already a no-op on MSVC targets.
