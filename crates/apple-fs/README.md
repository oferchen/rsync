# apple-fs

Filesystem primitives that `oc-rsync` needs in order to interact with Apple
platforms and with cross-platform formats originated by them.

## Surface

- `mkfifo` / `mknod` (Unix-only): safe wrappers around `mkfifo(3)` and
  `mknod(2)` used by the receiver when materialising FIFOs and device nodes.
  Mirror upstream rsync's `syscall.c:do_mknod`.
- `normalize_filename` (macOS-only NFC pass; identity stub elsewhere): used
  by the receiver and the local-copy delete pass to compare filenames between
  HFS+/APFS (NFD) and the rest of the world (NFC).
- `is_apple_double_name` / `apple_double_companion` (cross-platform): lexical
  helpers that pair a regular file `foo` with its AppleDouble sidecar `._foo`
  in either direction. Useful both as a filter primitive and as a building
  block for AppleDouble-aware tooling.
- `apple_double` module: a self-contained AppleDouble v2 (RFC 1740) parser
  and encoder. Pure-data, no FFI. Lets callers inspect or synthesise
  `._foo` containers in audit harnesses, tests, and any future merge
  implementation.
- `resource_fork` module: safe accessors for `com.apple.ResourceFork` and
  `com.apple.FinderInfo`. On macOS they delegate to the third-party `xattr`
  crate (the same safe wrapper used by `crates/metadata`); on every other
  target every accessor is a no-op stub that returns `Ok(None)` / `Ok(())`.

## Unsafe-code policy

The crate keeps `#![deny(unsafe_code)]`. macOS xattr syscalls (`getxattr(2)`,
`setxattr(2)`, `removexattr(2)`) are reached through the pre-vetted `xattr`
crate, which contains the only `unsafe` blocks involved. Per the workspace
unsafe-code policy, no FFI is added directly inside `apple-fs`.

## Integration with the transfer pipeline

End-to-end transfer of `com.apple.ResourceFork` / `com.apple.FinderInfo`
already happens via `metadata::xattr` and `protocol::xattr::wire`; see
`docs/audits/apple-fs-roundtrip.md` for the full audit. The accessors in
this crate are intentionally orthogonal to that pipeline and are intended
for tooling, audits, and any future opt-in AppleDouble merge feature
(audit follow-up F-2).

## References

- RFC 1740: "MIME Encapsulation of Macintosh Files - MacMIME", section 5
  (AppleDouble container layout).
- Apple Technical Note TN1188: "AppleSingle/AppleDouble Formats".
- Upstream rsync 3.4.1 `xattrs.c` - reads `com.apple.*` xattrs through the
  generic `getxattr` / `setxattr` path with no Mac-specific code.
