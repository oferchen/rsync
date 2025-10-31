# rsync-apple-fs

Utilities that provide the small set of filesystem primitives required by
`oc-rsync` when interacting with Apple platforms.

The upstream `rsync` implementation relies on the `mkfifo(3)` and `mknod(2)`
syscalls when creating named pipes or other special files while synchronising
directories.  The Rust standard library does not expose safe wrappers for these
operations, so this crate offers minimal bindings that mirror the behaviour of
the C routines while integrating with the rest of the Rust-based
implementation.

The public API intentionally remains tiny.  Consumers should prefer the higher
level abstractions in `rsync-core` wherever possible and use these helpers only
when direct access to the underlying syscalls is required.

