# flist

File list generation and traversal - mirrors upstream rsync's `flist.c` for
enumerating files, directories, and symlinks with deterministic ordering.

## Key Public Types

- `FileListBuilder` - configures traversal options (root emission, symlink following)
- `FileListWalker` - `Iterator` yielding `FileListEntry` in depth-first order
- `FileListEntry` - represents a discovered path with metadata
- `LazyEntry` - deferred-metadata entry for large directory trees
- `FileListError` - path-annotated I/O errors from traversal

## Modules

- `batched_stat` - parallel `fstatat`/`statx` batching for high file counts (Unix)
- `parallel` - rayon-based parallel traversal
- `symlink_safety` - cycle detection when following directory symlinks

## Dependencies

- **Upstream:** `logging` (diagnostics), `libc` (optional, for batched syscalls)
- **Downstream:** `engine`, `core`

## Features

- `parallel` - enables rayon + batched syscalls for parallel file enumeration
- `serde` - serialization support for file list types

## Platform Notes

- Unix: `batched_stat` module uses `fstatat`/`statx` for reduced syscall overhead
- Windows: falls back to standard `std::fs::metadata` per-entry
- All platforms: lexicographic sort ensures deterministic output regardless of FS iteration order
