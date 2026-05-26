# WIN-G.d: Windows GNU Exception-Handling Release Binary Implications

Follow-up to WIN-G.c (EH ABI compatibility status). This document answers
four user-facing questions about the `windows-gnu-eh` crate and the EH ABI
choice for shipped Windows binaries.

Last verified: 2026-05-26 against master.

## 1. Which target triples we ship and why

### Shipped target

All release binaries targeting Windows use a single triple:

    x86_64-pc-windows-msvc

The release workflow (`.github/workflows/release-cross.yml:616, 640, 670`)
builds, packages, and uploads only MSVC artifacts. Stable, beta, and
nightly toolchains are exercised, all producing MSVC-linked binaries.

### Why MSVC

Three factors drove the MSVC-only release decision:

1. **Runtime dependency surface.** MSVC-linked binaries depend on
   `vcruntime140.dll` and `ucrtbase.dll`, both shipped with every
   supported Windows version since Windows 10. GNU-linked binaries
   would additionally depend on `libgcc_s_seh-1.dll` and
   `libwinpthread-1.dll` from the MinGW runtime, requiring either a
   separate DLL bundle or a user-installed MinGW distribution.

2. **CI parity.** MSVC is the only triple with runtime test coverage.
   Three CI jobs exercise Windows MSVC builds (`windows-test`,
   `windows-iocp`, `windows-acl-xattr`) across stable, beta, and
   nightly toolchains. The GNU triple has only a `cargo check`
   compile-time validation job (`windows-gnu-cross-check`) with no
   runtime tests, no IOCP path coverage, and no ACL/xattr coverage.

3. **Ecosystem alignment.** The `windows` and `windows-sys` crates
   from microsoft/windows-rs are maintained and tested primarily
   against MSVC. Rust's official Windows support tiers list both MSVC
   and GNU as Tier 1, but the GitHub Actions `windows-latest` runner
   ships with the MSVC toolchain pre-installed, making it the
   path-of-least-resistance for CI.

### GNU target status

The `x86_64-pc-windows-gnu` triple remains in the workspace for
cross-compilation convenience. The `windows-gnu-cross-check` CI job
validates that the workspace compiles cleanly under MinGW, catching
accidental MSVC-only dependencies early. No GNU binaries are packaged
or shipped.

## 2. Panic unwinding across FFI boundaries

### On shipped MSVC binaries

Panic unwinding is safe. MSVC-targeted Rust uses Structured Exception
Handling (SEH) - the native Windows unwinding mechanism. All Win32 APIs
are called via `extern "system"` bindings from the `windows` and
`windows-sys` crates. Because both the Rust unwinder and the OS use SEH,
unwind frames are correctly chained.

Since Rust 1.71 (RFC 2945, stabilized in rust 1.71.0), a panic that
reaches an `extern "C"` or `extern "system"` boundary is defined to
abort the process rather than invoke undefined behavior. This guarantee
applies to the project's pinned toolchain (Rust 1.88.0).

In practice, no `extern "system"` callback in the codebase can panic:

- **`SetConsoleCtrlHandler` callback** (`platform/src/signal.rs:124`):
  the handler body performs only `OnceLock::get()` (returns `Option`,
  cannot panic) and `AtomicBool::store` (infallible). No allocation,
  no mutex, no fallible API.

- **SCM control handler** (`platform/src/windows_service.rs:200`):
  stores atomics and calls `SetServiceStatus` through the `windows`
  crate's safe wrapper. Cannot panic.

- **SCM service main** (`platform/src/windows_service.rs:151`): this
  is the one site where a user-provided callback (`ServiceMainCallback`)
  runs inside an `extern "system"` frame. If the callback panics,
  the process aborts per Rust 1.71+ semantics. This is the correct
  behavior for an unrecoverable service failure - the SCM detects the
  process exit and can restart the service per its recovery policy.

### On hypothetical GNU binaries

GNU-targeted Rust on x86_64 uses DWARF-based unwinding. Win32 APIs are
kernel transitions that do not push userspace unwind frames, so DWARF
unwinding around (not through) Win32 calls works in practice. The
`windows-gnu-eh` crate ensures DWARF frame registration symbols resolve
at link time. The Rust 1.71+ panic-through-FFI-aborts guarantee applies
equally to the GNU target, so there is no undefined behavior risk even
in the theoretical case.

### Summary

No panic unwinding safety concern exists for shipped binaries.

## 3. SetConsoleCtrlHandler and signal handling

### Question

Does the EH ABI choice (SEH on MSVC vs DWARF on GNU) affect the
`SetConsoleCtrlHandler` signal-handling path?

### Answer: no

The console control handler registered in `platform/src/signal.rs` is
an `extern "system"` function that:

1. Takes a `u32` control type.
2. Pattern-matches against `CTRL_C_EVENT`, `CTRL_CLOSE_EVENT`, and
   `CTRL_BREAK_EVENT`.
3. Stores `true` into pre-initialized `OnceLock<Arc<AtomicBool>>`
   statics.
4. Returns `BOOL(1)` (handled) or `BOOL(0)` (pass to next handler).

Every operation in the handler is infallible. No heap allocation occurs.
No mutex is acquired. No Rust code that could panic is reachable from
the handler body.

The handler's calling convention (`extern "system"`) is ABI-correct on
both MSVC and GNU targets. `SetConsoleCtrlHandler` dispatches the
callback from an OS-created thread using the standard Win32 calling
convention, which `extern "system"` maps to on all Windows targets.

The Windows Service Control Manager callbacks
(`service_main_entry`, `service_control_handler`) follow the same
pattern: `extern "system"` with infallible bodies (atomics + status
reporting). The one exception - `service_main_entry` invoking
`ServiceMainCallback` - is covered by Rust 1.71+ abort-on-panic
semantics.

### EH ABI irrelevance

Because no exception unwind can originate inside any of these callbacks,
the mechanism used to unwind (SEH vs DWARF) is never exercised. The
callbacks are functionally identical on MSVC and GNU targets.

## 4. User-facing implications

### For users of release binaries

**None.** Release binaries are MSVC-only. The `windows-gnu-eh` crate
compiles to a single `pub const fn force_link() {}` on MSVC targets,
which is optimized away entirely. It contributes zero bytes to the
final binary, introduces no runtime behavior, and has no effect on
panic handling, signal handling, or any user-visible feature.

### For users building from source

Users building on a Windows host with the default `rustup` toolchain
(`x86_64-pc-windows-msvc`) are in the same position as release binary
users - the crate is a no-op.

Users cross-compiling from Linux with `cargo-zigbuild` targeting
`x86_64-pc-windows-gnu` benefit from the crate: it resolves link errors
caused by the Zig toolchain omitting legacy libgcc entry points. The
resulting binary uses DWARF unwinding, which is functionally correct
for all current FFI call sites (see section 2).

### For daemon operators

The daemon (`oc-rsync --daemon`) and Windows service mode are unaffected
by EH ABI choice. Signal handling via `SetConsoleCtrlHandler` and SCM
control handlers work identically on both targets (see section 3). The
IOCP I/O dispatch fast path is gated on `target_os = "windows"` with
no `target_env` distinction, so it compiles and runs on both MSVC and
GNU - though only the MSVC path is tested in CI.

## 5. Recommendations

No action is required. The current configuration is correct:

- Release binaries ship MSVC, which uses SEH unwinding natively.
- The `windows-gnu-eh` crate is retained for cross-compilation support
  at zero cost to MSVC builds.
- All `extern "system"` callbacks are panic-free by construction.
- Rust 1.71+ guarantees abort (not UB) if a panic ever reaches an
  FFI boundary.

If the project decides to drop the GNU target entirely per the WIN-G.c
audit companion (`docs/audits/windows-gnu-vs-msvc.md`), the
`windows-gnu-eh` crate can be removed with no user-facing impact.

## 6. References

Code:

- `crates/windows-gnu-eh/src/lib.rs:1-243` (shim crate)
- `src/bin/oc-rsync.rs:15-17` (`force_link()` call site)
- `crates/platform/src/signal.rs:100-152` (Windows signal handlers)
- `crates/platform/src/windows_service.rs:151-189` (SCM service main)
- `crates/platform/src/windows_service.rs:200-220` (SCM control handler)
- `crates/fast_io/src/iocp/pump.rs` (IOCP completion pump)

CI / release:

- `.github/workflows/release-cross.yml:616, 640, 670` (MSVC release target)
- `.github/workflows/ci.yml` - `windows-test`, `windows-iocp`,
  `windows-acl-xattr` (MSVC CI jobs)
- `.github/workflows/ci.yml` - `windows-gnu-cross-check` (GNU compile-only)

Companion audits:

- `docs/audits/windows-gnu-vs-msvc.md` - drop-or-keep evaluation
- `docs/audits/windows-gnu-vs-msvc-evaluation.md` - long-form predecessor
- `docs/user/windows-support-matrix.md` - user-facing feature matrix
