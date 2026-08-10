# windows-gnu-eh

Compatibility shims for `x86_64-pc-windows-gnu` DWARF exception handling - provides
the `___register_frame_info` / `___deregister_frame_info` symbols missing from
Zig's Windows GNU toolchain.

## Purpose

When cross-compiling for `x86_64-pc-windows-gnu` (e.g., with `cargo-zigbuild`),
Zig omits legacy libgcc entry points that Rust's startup object (`rsbegin.o`)
references. This crate provides `#[no_mangle]` shim functions that forward at
runtime to the modern symbols from libunwind/libgcc, resolved lazily via
`LoadLibraryA`/`GetProcAddress` and cached in atomics.

## Key Functions

- `force_link()` - called from `main()` behind a `#[cfg(target_env = "gnu")]` gate
  to ensure the shim symbols are linked into the final binary. On non-GNU targets
  this compiles to a no-op that is optimized away.

## When This Crate Is Needed

**Only** when targeting `x86_64-pc-windows-gnu`. On all other targets (MSVC, Unix,
macOS) the crate compiles to nothing.

## Dependencies

- **Upstream:** none (uses only `core` and Windows kernel32 APIs)
- **Downstream:** `cli` (root binary calls `force_link()`)

## Maintenance

Minimal - approximately 180 lines of self-contained code. No updates required
unless Rust changes its startup object linking model for the GNU target.
